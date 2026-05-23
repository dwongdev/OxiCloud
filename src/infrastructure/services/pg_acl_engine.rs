//! PostgreSQL-backed implementation of `AuthorizationEngine`.
//!
//! Stores grants in `storage.access_grants` (see migration
//! `20260520000000_rebac_access_grants.sql`). Cascading is resolved at check
//! time via PostgreSQL `ltree` `@>` (ancestor-of) on `storage.folders.lpath`,
//! using the existing GiST index for O(log N) traversal.
//!
//! Owner is implicit — `storage.folders.user_id` / `storage.files.user_id`
//! are checked first via dedicated helpers; if the caller is the owner, no
//! SQL against `access_grants` happens.
//!
//! ## Lifecycle cleanup
//!
//! In v1, cleanup of grant rows when a resource or subject is permanently
//! deleted is enforced by **DB triggers** (`trg_cleanup_grants_*` in the
//! migration). The application layer does not call `revoke_all_for_*`
//! explicitly today — the triggers are the canonical path because they
//! also catch bulk SQL maintenance, admin scripts, and any code path that
//! bypasses the service layer.
//!
//! The `revoke_all_for_resource` / `revoke_all_for_subject` methods exist
//! on the trait for future use cases:
//! - **Caching** (planned) — a `CachedAuthorizationEngine` decorator needs
//!   to see the invalidation event at the engine boundary, not just at the
//!   SQL level. When caching lands, services will start calling these
//!   methods explicitly before/around delete operations.
//! - **Alternate engines** (OpenFGA, future) — engines that don't share a
//!   DB transaction with the resource table need an explicit signal to
//!   delete their tuples.

use std::sync::Arc;
use uuid::Uuid;

use sqlx::PgPool;

use crate::application::ports::authorization_ports::AuthorizationEngine;
use crate::common::errors::DomainError;
use crate::domain::services::authorization::{
    Grant, GrantCursor, IncomingGrantSummary, Permission, Resource, ResourceKind, Subject,
};
use crate::infrastructure::repositories::pg::file_blob_read_repository::FileBlobReadRepository;
use crate::infrastructure::repositories::pg::folder_db_repository::FolderDbRepository;

pub struct PgAclEngine {
    pool: Arc<PgPool>,
    folder_repo: Arc<FolderDbRepository>,
    file_repo: Arc<FileBlobReadRepository>,
}

impl PgAclEngine {
    pub fn new(
        pool: Arc<PgPool>,
        folder_repo: Arc<FolderDbRepository>,
        file_repo: Arc<FileBlobReadRepository>,
    ) -> Self {
        Self {
            pool,
            folder_repo,
            file_repo,
        }
    }

    /// Creates a stub instance for tests that need to construct services
    /// without a real PostgreSQL pool. Connecting to the lazy pool will
    /// fail at runtime — only safe in tests that exercise types, not actual
    /// authz queries.
    #[cfg(test)]
    pub fn new_stub() -> Self {
        let pool = sqlx::pool::PoolOptions::<sqlx::Postgres>::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid:5432/none")
            .unwrap();
        Self {
            pool: Arc::new(pool),
            folder_repo: Arc::new(FolderDbRepository::new_stub()),
            file_repo: Arc::new(FileBlobReadRepository::new_stub()),
        }
    }

    /// Returns the owner UUID for any resource type.
    async fn owner_of(&self, resource: Resource) -> Result<Uuid, DomainError> {
        match resource {
            Resource::Folder(id) => self.folder_repo.get_folder_user_id(&id.to_string()).await,
            Resource::File(id) => self.file_repo.get_file_user_id(&id.to_string()).await,
        }
    }

    /// Cascading check for folders: is there a grant on any ancestor folder
    /// (including the target itself) in this subject + permission?
    /// Uses GiST index on `storage.folders.lpath`.
    async fn folder_cascade_grant_exists(
        &self,
        subject: Subject,
        permission: Permission,
        folder_id: Uuid,
    ) -> Result<bool, DomainError> {
        let exists: Option<i32> = sqlx::query_scalar(
            r#"
            SELECT 1
              FROM storage.access_grants g
              JOIN storage.folders gf ON gf.id = g.resource_id
             WHERE g.subject_type  = $1
               AND g.subject_id    = $2
               AND g.permission    = $3
               AND g.resource_type = 'folder'
               AND gf.lpath @> (SELECT lpath FROM storage.folders WHERE id = $4)
             LIMIT 1
            "#,
        )
        .bind(subject.type_str())
        .bind(subject.id())
        .bind(permission.as_str())
        .bind(folder_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("folder cascade: {e}")))?;

        Ok(exists.is_some())
    }

    /// Cascading check for files: either a direct file grant OR a grant on
    /// any ancestor folder of the file's containing folder.
    async fn file_cascade_grant_exists(
        &self,
        subject: Subject,
        permission: Permission,
        file_id: Uuid,
    ) -> Result<bool, DomainError> {
        let exists: Option<i32> = sqlx::query_scalar(
            r#"
            SELECT 1
              FROM (
                -- direct file grant
                SELECT 1
                  FROM storage.access_grants
                 WHERE subject_type = $1 AND subject_id = $2 AND permission = $3
                   AND resource_type = 'file' AND resource_id = $4
                UNION ALL
                -- cascading from any ancestor folder of the file's containing folder
                SELECT 1
                  FROM storage.access_grants g
                  JOIN storage.folders gf     ON gf.id = g.resource_id
                  JOIN storage.files target_f ON target_f.id = $4
                 WHERE g.subject_type  = $1
                   AND g.subject_id    = $2
                   AND g.permission    = $3
                   AND g.resource_type = 'folder'
                   AND target_f.folder_id IS NOT NULL
                   AND gf.lpath @> (SELECT lpath FROM storage.folders
                                     WHERE id = target_f.folder_id)
              ) any_match
             LIMIT 1
            "#,
        )
        .bind(subject.type_str())
        .bind(subject.id())
        .bind(permission.as_str())
        .bind(file_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("file cascade: {e}")))?;

        Ok(exists.is_some())
    }

    /// Look up a single grant by id. Returns `(resource, granted_by)` so
    /// the REST `DELETE /api/grants/{id}` handler can decide authorization
    /// without a second round-trip. Returns `Ok(None)` if no such grant.
    pub async fn find_grant_by_id(
        &self,
        grant_id: Uuid,
    ) -> Result<Option<(Resource, Uuid)>, DomainError> {
        let row: Option<(String, Uuid, Uuid)> = sqlx::query_as(
            "SELECT resource_type, resource_id, granted_by FROM storage.access_grants WHERE id = $1",
        )
        .bind(grant_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("find_grant_by_id: {e}")))?;

        let Some((rt, rid, granter)) = row else {
            return Ok(None);
        };
        let res = Resource::from_parts(&rt, rid)
            .ok_or_else(|| DomainError::internal_error("PgAcl", "unknown resource_type"))?;
        Ok(Some((res, granter)))
    }

    /// Decode a (id, subject_type, subject_id, resource_type, resource_id,
    /// permission, granted_by, granted_at) row into a `Grant`.
    fn row_to_grant(
        row: (
            Uuid,
            String,
            Uuid,
            String,
            Uuid,
            String,
            Uuid,
            chrono::DateTime<chrono::Utc>,
        ),
    ) -> Result<Grant, DomainError> {
        let subject = Subject::from_parts(&row.1, row.2)
            .ok_or_else(|| DomainError::internal_error("PgAcl", "unknown subject_type"))?;
        let resource = Resource::from_parts(&row.3, row.4)
            .ok_or_else(|| DomainError::internal_error("PgAcl", "unknown resource_type"))?;
        let permission = Permission::parse(&row.5)
            .ok_or_else(|| DomainError::internal_error("PgAcl", "unknown permission"))?;
        Ok(Grant {
            id: row.0,
            subject,
            resource,
            permission,
            granted_by: row.6,
            granted_at: row.7,
        })
    }
}

impl AuthorizationEngine for PgAclEngine {
    async fn check(
        &self,
        subject: Subject,
        permission: Permission,
        resource: Resource,
    ) -> Result<bool, DomainError> {
        // Owner short-circuit (only for User subjects — groups/tokens/external
        // are never owners of resources).
        if let Subject::User(uid) = subject {
            match self.owner_of(resource).await {
                Ok(owner) if owner == uid => return Ok(true),
                Ok(_) => { /* not owner — fall through to grants */ }
                Err(e) if e.kind == crate::common::errors::ErrorKind::NotFound => {
                    // Resource doesn't exist — no permission. Return false
                    // rather than propagating NotFound; the caller (`require`)
                    // converts a false back to NotFound on its own.
                    return Ok(false);
                }
                Err(e) => return Err(e),
            }
        }

        // Cascading grant check.
        match resource {
            Resource::Folder(id) => {
                self.folder_cascade_grant_exists(subject, permission, id)
                    .await
            }
            Resource::File(id) => {
                self.file_cascade_grant_exists(subject, permission, id)
                    .await
            }
        }
    }

    async fn list_incoming_grants(
        &self,
        subject: Subject,
        permission_filter: Option<Permission>,
    ) -> Result<Vec<Grant>, DomainError> {
        let perm_str = permission_filter.map(|p| p.as_str().to_string());

        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                chrono::DateTime<chrono::Utc>,
            ),
        >(
            r#"
            SELECT id, subject_type, subject_id, resource_type, resource_id,
                   permission, granted_by, granted_at
              FROM storage.access_grants
             WHERE subject_type = $1
               AND subject_id   = $2
               AND ($3::text IS NULL OR permission = $3)
             ORDER BY granted_at DESC
            "#,
        )
        .bind(subject.type_str())
        .bind(subject.id())
        .bind(perm_str)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("list incoming: {e}")))?;

        rows.into_iter().map(Self::row_to_grant).collect()
    }

    async fn list_incoming_resources_paged(
        &self,
        subject: Subject,
        kinds: &[ResourceKind],
        limit: u32,
        cursor: Option<GrantCursor>,
    ) -> Result<(Vec<IncomingGrantSummary>, Option<GrantCursor>), DomainError> {
        // Build kind filter array — NULL means "all kinds".
        let kind_strs: Option<Vec<&str>> = if kinds.is_empty() {
            None
        } else {
            Some(kinds.iter().map(|k| k.as_str()).collect())
        };

        let cursor_at = cursor.as_ref().map(|c| c.granted_at);
        let cursor_id = cursor.as_ref().map(|c| c.resource_id);

        // Fetch limit+1 rows so we can detect whether a next page exists.
        let fetch_limit = (limit as i64) + 1;

        // Each row: (resource_type, resource_id, permissions_text_array,
        //             granted_at, granted_by)
        type Row = (
            String,
            Uuid,
            Vec<String>,
            chrono::DateTime<chrono::Utc>,
            Uuid,
        );

        let rows: Vec<Row> = sqlx::query_as(
            r#"
            WITH agg AS (
                SELECT
                    resource_type,
                    resource_id,
                    array_agg(DISTINCT permission ORDER BY permission) AS permissions,
                    MIN(granted_at)                                    AS granted_at,
                    (array_agg(granted_by ORDER BY granted_at))[1]    AS granted_by
                FROM storage.access_grants
                WHERE subject_type = $1
                  AND subject_id   = $2
                  AND ($3::text[] IS NULL OR resource_type = ANY($3))
                GROUP BY resource_type, resource_id
            )
            SELECT resource_type, resource_id, permissions, granted_at, granted_by
            FROM agg
            WHERE (  $4::timestamptz IS NULL
                  OR granted_at < $4
                  OR (granted_at = $4 AND resource_id < $5::uuid))
            ORDER BY granted_at DESC, resource_id DESC
            LIMIT $6
            "#,
        )
        .bind(subject.type_str())
        .bind(subject.id())
        .bind(kind_strs)
        .bind(cursor_at)
        .bind(cursor_id)
        .bind(fetch_limit)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| {
            DomainError::internal_error("PgAcl", format!("list_incoming_resources_paged: {e}"))
        })?;

        let has_next = rows.len() > limit as usize;
        let rows: Vec<Row> = rows.into_iter().take(limit as usize).collect();

        // Determine the next cursor from the last item we're actually returning.
        let next_cursor = if has_next {
            rows.last().map(|r| GrantCursor {
                granted_at: r.3,
                resource_id: r.1,
            })
        } else {
            None
        };

        // Convert rows into domain summaries.
        let summaries = rows
            .into_iter()
            .filter_map(|(rt, rid, perms_str, granted_at, granted_by)| {
                let resource_type = ResourceKind::parse(&rt)?;
                let permissions = perms_str
                    .iter()
                    .filter_map(|s| Permission::parse(s))
                    .collect();
                Some(IncomingGrantSummary {
                    resource_type,
                    resource_id: rid,
                    permissions,
                    granted_at,
                    granted_by,
                })
            })
            .collect();

        Ok((summaries, next_cursor))
    }

    async fn list_grants_on_resource(&self, resource: Resource) -> Result<Vec<Grant>, DomainError> {
        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                chrono::DateTime<chrono::Utc>,
            ),
        >(
            r#"
            SELECT id, subject_type, subject_id, resource_type, resource_id,
                   permission, granted_by, granted_at
              FROM storage.access_grants
             WHERE resource_type = $1
               AND resource_id   = $2
             ORDER BY granted_at DESC
            "#,
        )
        .bind(resource.type_str())
        .bind(resource.id())
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("list on resource: {e}")))?;

        rows.into_iter().map(Self::row_to_grant).collect()
    }

    async fn list_outgoing_grants(&self, granted_by: Uuid) -> Result<Vec<Grant>, DomainError> {
        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                chrono::DateTime<chrono::Utc>,
            ),
        >(
            r#"
            SELECT id, subject_type, subject_id, resource_type, resource_id,
                   permission, granted_by, granted_at
              FROM storage.access_grants
             WHERE granted_by = $1
             ORDER BY granted_at DESC
            "#,
        )
        .bind(granted_by)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("list outgoing: {e}")))?;

        rows.into_iter().map(Self::row_to_grant).collect()
    }

    async fn grant(
        &self,
        granted_by: Uuid,
        subject: Subject,
        permission: Permission,
        resource: Resource,
    ) -> Result<Grant, DomainError> {
        // Idempotent: ON CONFLICT DO UPDATE so we always return the row
        // (whether newly inserted or pre-existing). The "update" is a no-op
        // (granted_by/granted_at preserved from the existing row).
        let row = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                chrono::DateTime<chrono::Utc>,
            ),
        >(
            r#"
            INSERT INTO storage.access_grants
                (subject_type, subject_id, resource_type, resource_id, permission, granted_by)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (subject_type, subject_id, resource_type, resource_id, permission)
            DO UPDATE SET subject_type = EXCLUDED.subject_type
            RETURNING id, subject_type, subject_id, resource_type, resource_id,
                      permission, granted_by, granted_at
            "#,
        )
        .bind(subject.type_str())
        .bind(subject.id())
        .bind(resource.type_str())
        .bind(resource.id())
        .bind(permission.as_str())
        .bind(granted_by)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("insert grant: {e}")))?;

        Self::row_to_grant(row)
    }

    async fn revoke(&self, grant_id: Uuid) -> Result<(), DomainError> {
        sqlx::query("DELETE FROM storage.access_grants WHERE id = $1")
            .bind(grant_id)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| DomainError::internal_error("PgAcl", format!("revoke: {e}")))?;
        Ok(())
    }

    async fn revoke_all_for_resource(&self, resource: Resource) -> Result<usize, DomainError> {
        let result = sqlx::query(
            "DELETE FROM storage.access_grants WHERE resource_type = $1 AND resource_id = $2",
        )
        .bind(resource.type_str())
        .bind(resource.id())
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("revoke for resource: {e}")))?;

        Ok(result.rows_affected() as usize)
    }

    async fn revoke_all_for_subject(&self, subject: Subject) -> Result<usize, DomainError> {
        let result = sqlx::query(
            "DELETE FROM storage.access_grants WHERE subject_type = $1 AND subject_id = $2",
        )
        .bind(subject.type_str())
        .bind(subject.id())
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("revoke for subject: {e}")))?;

        Ok(result.rows_affected() as usize)
    }
}
