//! REST handlers for the ReBAC grant management endpoints.
//!
//! All endpoints under `/api/grants`. The authenticated caller is taken from
//! the `AuthUser` extractor. Authorization for sharing operations is enforced
//! via `authz.require(caller, Share, resource)` — handlers never embed their
//! own checks (see CLAUDE.md § Authorization).

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use futures::future::join_all;
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info, warn};
use utoipa::IntoParams;
use uuid::Uuid;

use crate::application::dtos::grant_dto::{
    CreateGrantDto, GrantDto, PermissionDto, ResourceDto, ResourceTypeDto, SharedWithMeDto,
    SharedWithMeItemDto, SharedWithMeQuery, SubjectDto, UpdateRoleDto,
};
use crate::application::ports::authorization_ports::AuthorizationEngine;
use crate::application::ports::file_ports::FileRetrievalUseCase;
use crate::application::ports::folder_ports::FolderUseCase;
use crate::common::di::AppState;
#[allow(unused_imports)]
use crate::common::errors::DomainError;
use crate::domain::errors::ErrorKind;
use crate::domain::services::authorization::{
    GrantCursor, IncomingGrantSummary, Permission, Resource, ResourceKind, Subject,
};
use crate::interfaces::errors::AppError;
use crate::interfaces::middleware::auth::AuthUser;

type AppStateRef = Arc<AppState>;

// ════════════════════════════════════════════════════════════════════════════
// POST /api/grants
// ════════════════════════════════════════════════════════════════════════════

#[utoipa::path(
    post,
    path = "/api/grants",
    request_body = CreateGrantDto,
    responses(
        (status = 201, description = "Grant(s) created", body = Vec<GrantDto>),
        (status = 400, description = "Invalid input (both/neither of permissions+role provided)"),
        (status = 404, description = "Resource not found OR caller lacks Share permission"),
    ),
    tag = "grants"
)]
pub async fn create_grant(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
    Json(dto): Json<CreateGrantDto>,
) -> impl IntoResponse {
    let authz = &state.authorization;
    let caller_id = auth_user.id;

    // Validate: exactly one of permissions/role
    let permissions: Vec<Permission> = match (dto.permissions, dto.role) {
        (Some(perms), None) if !perms.is_empty() => perms.into_iter().map(Into::into).collect(),
        (None, Some(role)) => role.expand().to_vec(),
        (Some(_), Some(_)) => {
            return AppError::new(
                StatusCode::BAD_REQUEST,
                "Provide either 'permissions' or 'role', not both",
                "InvalidInput",
            )
            .into_response();
        }
        _ => {
            return AppError::new(
                StatusCode::BAD_REQUEST,
                "Either 'permissions' (non-empty) or 'role' is required",
                "InvalidInput",
            )
            .into_response();
        }
    };

    let subject: Subject = dto.subject.into();
    let resource: Resource = dto.resource.into();

    // Caller must have Share on the resource (owners pass via short-circuit).
    if let Err(e) = authz
        .require(Subject::User(caller_id), Permission::Share, resource)
        .await
    {
        return AppError::from(e).into_response();
    }

    let mut results: Vec<GrantDto> = Vec::with_capacity(permissions.len());
    for perm in permissions {
        match authz.grant(caller_id, subject, perm, resource).await {
            Ok(grant) => results.push(grant.into()),
            Err(err) => {
                error!("grant insert failed for {perm:?}: {err}");
                return AppError::from(err).into_response();
            }
        }
    }
    info!(
        "Created {} grant(s) for subject={:?} on resource={:?} by user {}",
        results.len(),
        subject,
        resource,
        caller_id
    );
    (StatusCode::CREATED, Json(results)).into_response()
}

// ════════════════════════════════════════════════════════════════════════════
// DELETE /api/grants/{id}
// ════════════════════════════════════════════════════════════════════════════

#[utoipa::path(
    delete,
    path = "/api/grants/{id}",
    params(("id" = String, Path, description = "Grant UUID")),
    responses(
        (status = 204, description = "Grant revoked (or did not exist)"),
        (status = 404, description = "Caller lacks Share permission on the underlying resource"),
    ),
    tag = "grants"
)]
pub async fn revoke_grant(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let authz = &state.authorization;
    let caller_id = auth_user.id;
    let grant_id = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => return AppError::not_found(format!("Grant {id} not found")).into_response(),
    };

    // Look up the grant to find the underlying resource (and granter).
    let on_resource = match authz.find_grant_by_id(grant_id).await {
        Ok(Some((res, granter))) => (res, granter),
        Ok(None) => return StatusCode::NO_CONTENT.into_response(), // idempotent
        Err(e) => return AppError::from(e).into_response(),
    };

    // Caller is authorized if they are the granter OR have Share on the resource.
    if on_resource.1 != caller_id
        && let Err(e) = authz
            .require(Subject::User(caller_id), Permission::Share, on_resource.0)
            .await
    {
        return AppError::from(e).into_response();
    }

    if let Err(e) = authz.revoke(grant_id).await {
        return AppError::from(e).into_response();
    }
    info!("Revoked grant {grant_id} (caller {caller_id})");
    StatusCode::NO_CONTENT.into_response()
}

// ════════════════════════════════════════════════════════════════════════════
// PUT /api/grants/role
// ════════════════════════════════════════════════════════════════════════════

#[utoipa::path(
    put,
    path = "/api/grants/role",
    request_body = UpdateRoleDto,
    responses(
        (status = 200, description = "Role applied; returns the new full grant set", body = Vec<GrantDto>),
        (status = 404, description = "Resource not found or caller lacks Share"),
    ),
    tag = "grants"
)]
pub async fn set_role(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
    Json(dto): Json<UpdateRoleDto>,
) -> impl IntoResponse {
    let authz = &state.authorization;
    let caller_id = auth_user.id;
    let subject: Subject = dto.subject.into();
    let resource: Resource = dto.resource.into();
    let target_perms: std::collections::HashSet<Permission> =
        dto.role.expand().iter().copied().collect();

    // Caller must have Share on the resource.
    if let Err(e) = authz
        .require(Subject::User(caller_id), Permission::Share, resource)
        .await
    {
        return AppError::from(e).into_response();
    }

    // Fetch current grants on the resource for this subject.
    let current = match authz.list_grants_on_resource(resource).await {
        Ok(g) => g,
        Err(e) => return AppError::from(e).into_response(),
    };
    let current_perms: std::collections::HashSet<Permission> = current
        .iter()
        .filter(|g| g.subject == subject)
        .map(|g| g.permission)
        .collect();

    // Diff and apply.
    let to_add: Vec<Permission> = target_perms.difference(&current_perms).copied().collect();
    let to_remove: Vec<Permission> = current_perms.difference(&target_perms).copied().collect();

    for perm in &to_remove {
        if let Some(g) = current
            .iter()
            .find(|g| g.subject == subject && g.permission == *perm)
            && let Err(e) = authz.revoke(g.id).await
        {
            return AppError::from(e).into_response();
        }
    }
    for perm in &to_add {
        if let Err(e) = authz.grant(caller_id, subject, *perm, resource).await {
            return AppError::from(e).into_response();
        }
    }

    // Return the new full set.
    let after = match authz.list_grants_on_resource(resource).await {
        Ok(g) => g,
        Err(e) => return AppError::from(e).into_response(),
    };
    let mine: Vec<GrantDto> = after
        .into_iter()
        .filter(|g| g.subject == subject)
        .map(Into::into)
        .collect();

    info!(
        "Role applied: caller={} subject={:?} resource={:?} added={:?} removed={:?}",
        caller_id, subject, resource, to_add, to_remove
    );
    (StatusCode::OK, Json(mine)).into_response()
}

// ════════════════════════════════════════════════════════════════════════════
// GET /api/grants/incoming
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize, IntoParams)]
pub struct IncomingQuery {
    #[serde(default)]
    pub permission: Option<PermissionDto>,
}

#[utoipa::path(
    get,
    path = "/api/grants/incoming",
    params(IncomingQuery),
    responses(
        (status = 200, description = "Direct grants targeting the caller", body = Vec<GrantDto>),
    ),
    tag = "grants"
)]
pub async fn list_incoming(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
    Query(q): Query<IncomingQuery>,
) -> impl IntoResponse {
    let caller_id = auth_user.id;
    match state
        .authorization
        .list_incoming_grants(Subject::User(caller_id), q.permission.map(Into::into))
        .await
    {
        Ok(grants) => {
            let dtos: Vec<GrantDto> = grants.into_iter().map(Into::into).collect();
            (StatusCode::OK, Json(dtos)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// GET /api/grants/incoming/resources
// ════════════════════════════════════════════════════════════════════════════

#[utoipa::path(
    get,
    path = "/api/grants/incoming/resources",
    params(SharedWithMeQuery),
    responses(
        (status = 200,
         description = "Cursor-paginated resources shared with the caller. \
                        Each item carries the full file or folder details plus \
                        aggregated permissions. `next_cursor` is absent on the \
                        last page.",
         body = SharedWithMeDto),
    ),
    tag = "grants"
)]
pub async fn list_shared_with_me(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
    Query(q): Query<SharedWithMeQuery>,
) -> impl IntoResponse {
    let caller_id = auth_user.id;
    let subject = Subject::User(caller_id);

    // Parse resource_types filter (unknown values silently ignored).
    let kinds: Vec<ResourceKind> = q
        .resource_types
        .as_deref()
        .map(|s| {
            s.split(',')
                .filter_map(|t| ResourceKind::parse(t.trim()))
                .collect()
        })
        .unwrap_or_default();

    // Clamp limit to 1–200.
    let limit = q.limit.clamp(1, 200);

    // Decode cursor (treat invalid cursor as "start from top").
    let cursor = q.cursor.as_deref().and_then(GrantCursor::decode);

    // Fetch paged summaries from the ACL engine.
    let (summaries, next_cursor) = match state
        .authorization
        .list_incoming_resources_paged(subject, &kinds, limit, cursor)
        .await
    {
        Ok(r) => r,
        Err(e) => return AppError::from(e).into_response(),
    };

    // Split summaries by resource kind for parallel resolution.
    let file_summaries: Vec<&IncomingGrantSummary> = summaries
        .iter()
        .filter(|s| matches!(s.resource_type, ResourceKind::File))
        .collect();
    let folder_summaries: Vec<&IncomingGrantSummary> = summaries
        .iter()
        .filter(|s| matches!(s.resource_type, ResourceKind::Folder))
        .collect();

    let file_service = &state.applications.file_retrieval_service;
    let folder_service = &state.applications.folder_service_concrete;

    // Pre-compute ID strings to avoid temporaries inside async closures.
    let file_ids: Vec<String> = file_summaries
        .iter()
        .map(|s| s.resource_id.to_string())
        .collect();
    let folder_ids: Vec<String> = folder_summaries
        .iter()
        .map(|s| s.resource_id.to_string())
        .collect();

    // Resolve resource details concurrently (files and folders in parallel).
    let (file_results, folder_results) = tokio::join!(
        join_all(file_ids.iter().map(|id| file_service.get_file(id))),
        join_all(folder_ids.iter().map(|id| folder_service.get_folder(id)))
    );

    // Build the unified item list in original grant order (newest first).
    // We iterate summaries in order and pick the resolved result from the
    // appropriate typed bucket.
    let mut file_idx = 0usize;
    let mut folder_idx = 0usize;

    let mut items: Vec<SharedWithMeItemDto> = Vec::with_capacity(summaries.len());

    for summary in &summaries {
        match summary.resource_type {
            ResourceKind::File => {
                let result = &file_results[file_idx];
                file_idx += 1;
                match result {
                    Ok(file_dto) => {
                        items.push(SharedWithMeItemDto {
                            resource_type: ResourceTypeDto::File,
                            permissions: summary.permissions.iter().map(|p| (*p).into()).collect(),
                            granted_at: summary.granted_at,
                            granted_by: summary.granted_by,
                            file: Some(file_dto.clone().without_hierarchy_info()),
                            folder: None,
                        });
                    }
                    Err(e) if e.kind == ErrorKind::NotFound => {
                        // Stale grant (file deleted, trigger not yet fired) — skip silently.
                        warn!(
                            "Skipping stale file grant for resource_id={}: not found",
                            summary.resource_id
                        );
                    }
                    Err(e) => {
                        return AppError::internal_error(format!(
                            "Failed to fetch file {}: {e}",
                            summary.resource_id
                        ))
                        .into_response();
                    }
                }
            }
            ResourceKind::Folder => {
                let result = &folder_results[folder_idx];
                folder_idx += 1;
                match result {
                    Ok(folder_dto) => {
                        items.push(SharedWithMeItemDto {
                            resource_type: ResourceTypeDto::Folder,
                            permissions: summary.permissions.iter().map(|p| (*p).into()).collect(),
                            granted_at: summary.granted_at,
                            granted_by: summary.granted_by,
                            file: None,
                            folder: Some(folder_dto.clone().without_hierarchy_info()),
                        });
                    }
                    Err(e) if e.kind == ErrorKind::NotFound => {
                        warn!(
                            "Skipping stale folder grant for resource_id={}: not found",
                            summary.resource_id
                        );
                    }
                    Err(e) => {
                        return AppError::internal_error(format!(
                            "Failed to fetch folder {}: {e}",
                            summary.resource_id
                        ))
                        .into_response();
                    }
                }
            }
        }
    }

    (
        StatusCode::OK,
        Json(SharedWithMeDto {
            items,
            next_cursor: next_cursor.map(|c| c.encode()),
        }),
    )
        .into_response()
}

// ════════════════════════════════════════════════════════════════════════════
// GET /api/grants/outgoing
// ════════════════════════════════════════════════════════════════════════════

#[utoipa::path(
    get,
    path = "/api/grants/outgoing",
    responses(
        (status = 200, description = "Grants the caller has created", body = Vec<GrantDto>),
    ),
    tag = "grants"
)]
pub async fn list_outgoing(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
) -> impl IntoResponse {
    let caller_id = auth_user.id;
    match state.authorization.list_outgoing_grants(caller_id).await {
        Ok(grants) => {
            let dtos: Vec<GrantDto> = grants.into_iter().map(Into::into).collect();
            (StatusCode::OK, Json(dtos)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// GET /api/grants?resource_type=...&resource_id=...
// (list grants on a specific resource — requires Share on it)
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize, IntoParams)]
pub struct OnResourceQuery {
    pub resource_type: ResourceTypeDto,
    pub resource_id: Uuid,
}

#[utoipa::path(
    get,
    path = "/api/grants",
    params(OnResourceQuery),
    responses(
        (status = 200, description = "Grants on the specified resource", body = Vec<GrantDto>),
        (status = 404, description = "Resource not found or caller lacks Share"),
    ),
    tag = "grants"
)]
pub async fn list_on_resource(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
    Query(q): Query<OnResourceQuery>,
) -> impl IntoResponse {
    let authz = &state.authorization;
    let caller_id = auth_user.id;
    let resource: Resource = ResourceDto {
        kind: q.resource_type,
        id: q.resource_id,
    }
    .into();

    if let Err(e) = authz
        .require(Subject::User(caller_id), Permission::Share, resource)
        .await
    {
        return AppError::from(e).into_response();
    }

    match authz.list_grants_on_resource(resource).await {
        Ok(grants) => {
            let dtos: Vec<GrantDto> = grants.into_iter().map(Into::into).collect();
            (StatusCode::OK, Json(dtos)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

// Silence unused-import warnings for SubjectDto when only certain endpoints
// touch it directly.
#[allow(dead_code)]
fn _ensure_subject_dto_compiles(_: SubjectDto) {}
