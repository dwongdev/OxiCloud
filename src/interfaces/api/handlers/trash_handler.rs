use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::json;
use tracing::{debug, error, instrument, warn};

use crate::application::dtos::trash_dto::{TrashResourcesDto, TrashResourcesQuery};
use crate::application::ports::trash_ports::TrashUseCase;
use crate::common::di::AppState;
use crate::interfaces::errors::AppError;
use crate::interfaces::middleware::auth::AuthUser;
use std::sync::Arc;

/// Cursor-paginated list of a user's trashed resources.
///
/// Sorts by `deletion_date` (default — soonest expiry first), `trashed_at`
/// (most recently trashed first), `name`, `type`, or `size`. Filter on
/// `resource_types=file` or `resource_types=folder` to narrow to one kind.
/// Items implicitly trashed as descendants of a trashed parent are excluded
/// (only top-level trashed items appear).
#[utoipa::path(
    get,
    path = "/api/trash/resources",
    params(TrashResourcesQuery),
    responses(
        (status = 200, description = "Paginated list of trashed resources",
         body = crate::application::dtos::trash_dto::TrashResourcesDto),
        (status = 400, description = "Invalid cursor or query parameters"),
        (status = 501, description = "Trash feature not enabled"),
    ),
    security(("bearerAuth" = [])),
    tag = "trash"
)]
#[instrument(skip_all)]
pub async fn get_trash_resources(
    State(state): State<Arc<AppState>>,
    auth_user: AuthUser,
    Query(q): Query<TrashResourcesQuery>,
) -> axum::response::Response {
    let user_id = auth_user.id;

    let trash_service = match state.trash_service.as_ref() {
        Some(service) => service,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({ "error": "Trash feature is not enabled" })),
            )
                .into_response();
        }
    };

    let order_by = q.order_by.as_deref().unwrap_or("deletion_date").to_owned();

    // Discard cursor if sort dimension or direction changed between pages.
    let cursor = q
        .decode_cursor()
        .filter(|c| c.order_by == order_by && c.reverse == q.reverse);

    let kinds = q.resource_kinds();

    match trash_service
        .list_resources_paged(
            user_id,
            q.limit_clamped(),
            cursor,
            &order_by,
            kinds.as_deref(),
            q.reverse,
        )
        .await
    {
        Ok((items, next_cursor)) => (
            StatusCode::OK,
            Json(TrashResourcesDto::with_cursor(items, next_cursor)),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Moves a file to the trash
#[utoipa::path(
    delete,
    path = "/api/trash/files/{id}",
    params(("id" = String, Path, description = "File ID")),
    responses(
        (status = 200, description = "File moved to trash"),
        (status = 501, description = "Trash feature not enabled")
    ),
    security(("bearerAuth" = [])),
    tag = "trash"
)]
#[instrument(skip_all)]
pub async fn move_file_to_trash(
    State(state): State<Arc<AppState>>,
    auth_user: AuthUser,
    Path(item_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let user_id = auth_user.id;
    debug!(
        "Request to move file to trash: id={}, user={}",
        item_id, user_id
    );

    let trash_service = match state.trash_service.as_ref() {
        Some(service) => service,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({
                    "error": "Trash feature is not enabled"
                })),
            );
        }
    };

    // Specify that it is a file
    let result = trash_service.move_to_trash(&item_id, "file", user_id).await;

    match result {
        Ok(_) => {
            debug!("File moved to trash successfully");
            (
                StatusCode::OK,
                Json(json!({
                    "success": true,
                    "message": "File moved to trash successfully"
                })),
            )
        }
        Err(e) => {
            error!("Error moving file to trash: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "Error moving file to trash"
                })),
            )
        }
    }
}

/// Moves a folder to the trash
#[utoipa::path(
    delete,
    path = "/api/trash/folders/{id}",
    params(("id" = String, Path, description = "Folder ID")),
    responses(
        (status = 200, description = "Folder moved to trash"),
        (status = 501, description = "Trash feature not enabled")
    ),
    security(("bearerAuth" = [])),
    tag = "trash"
)]
#[instrument(skip_all)]
pub async fn move_folder_to_trash(
    State(state): State<Arc<AppState>>,
    auth_user: AuthUser,
    Path(item_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let user_id = auth_user.id;
    debug!(
        "Request to move folder to trash: id={}, user={}",
        item_id, user_id
    );

    let trash_service = match state.trash_service.as_ref() {
        Some(service) => service,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({
                    "error": "Trash feature is not enabled"
                })),
            );
        }
    };

    // Specify that it is a folder
    let result = trash_service
        .move_to_trash(&item_id, "folder", user_id)
        .await;

    match result {
        Ok(_) => {
            debug!("Folder moved to trash successfully");
            (
                StatusCode::OK,
                Json(json!({
                    "success": true,
                    "message": "Folder moved to trash successfully"
                })),
            )
        }
        Err(e) => {
            error!("Error moving folder to trash: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "Error moving folder to trash"
                })),
            )
        }
    }
}

/// Restores an item from the trash to its original location
#[utoipa::path(
    post,
    path = "/api/trash/{id}/restore",
    params(("id" = String, Path, description = "Trash item ID")),
    responses(
        (status = 200, description = "Item restored from trash"),
        (status = 501, description = "Trash feature not enabled")
    ),
    security(("bearerAuth" = [])),
    tag = "trash"
)]
#[instrument(skip_all)]
pub async fn restore_from_trash(
    State(state): State<Arc<AppState>>,
    auth_user: AuthUser,
    Path(trash_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    debug!("Request to restore item {} from trash", trash_id);

    let trash_service = match state.trash_service.as_ref() {
        Some(service) => service,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({
                    "error": "Trash feature is not enabled"
                })),
            );
        }
    };
    let result = trash_service.restore_item(&trash_id, auth_user.id).await;

    match result {
        Ok(_) => {
            debug!("Item restored successfully");
            (
                StatusCode::OK,
                Json(json!({
                    "success": true,
                    "message": "Item restored successfully"
                })),
            )
        }
        Err(e) => {
            let err_str = format!("{}", e);
            // If item not found, report success (it was already restored or removed)
            if err_str.contains("not found") || err_str.contains("NotFound") {
                warn!(
                    "Item not found in trash, but reporting success: {}",
                    trash_id
                );
                return (
                    StatusCode::OK,
                    Json(json!({
                        "success": true,
                        "message": "Item restored (or was already removed from trash)"
                    })),
                );
            }

            error!("Error restoring item from trash: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "Error restoring item from trash"
                })),
            )
        }
    }
}

/// Permanently deletes an item from the trash
#[utoipa::path(
    delete,
    path = "/api/trash/{id}",
    params(("id" = String, Path, description = "Trash item ID")),
    responses(
        (status = 200, description = "Item permanently deleted"),
        (status = 501, description = "Trash feature not enabled")
    ),
    security(("bearerAuth" = [])),
    tag = "trash"
)]
#[instrument(skip_all)]
pub async fn delete_permanently(
    State(state): State<Arc<AppState>>,
    auth_user: AuthUser,
    Path(trash_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    debug!("Request to permanently delete item {}", trash_id);

    let trash_service = match state.trash_service.as_ref() {
        Some(service) => service,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({
                    "error": "Trash feature is not enabled"
                })),
            );
        }
    };
    let result = trash_service
        .delete_permanently(&trash_id, auth_user.id)
        .await;

    match result {
        Ok(_) => {
            debug!("Item permanently deleted");
            (
                StatusCode::OK,
                Json(json!({
                    "success": true,
                    "message": "Item deleted permanently"
                })),
            )
        }
        Err(e) => {
            let err_str = format!("{}", e);
            // If item not found, report success (it was already deleted)
            if err_str.contains("not found") || err_str.contains("NotFound") {
                warn!(
                    "Item not found in trash, but reporting success: {}",
                    trash_id
                );
                return (
                    StatusCode::OK,
                    Json(json!({
                        "success": true,
                        "message": "Item deleted (or was already removed from trash)"
                    })),
                );
            }

            error!("Error permanently deleting item: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "Error deleting item permanently"
                })),
            )
        }
    }
}

/// Empties the trash completely for the current user
#[utoipa::path(
    delete,
    path = "/api/trash/empty",
    responses(
        (status = 200, description = "Trash emptied successfully"),
        (status = 501, description = "Trash feature not enabled")
    ),
    security(("bearerAuth" = [])),
    tag = "trash"
)]
#[instrument(skip_all)]
pub async fn empty_trash(
    State(state): State<Arc<AppState>>,
    auth_user: AuthUser,
) -> (StatusCode, Json<serde_json::Value>) {
    debug!("Request to empty trash for user {}", auth_user.id);

    let trash_service = match state.trash_service.as_ref() {
        Some(service) => service,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({
                    "error": "Trash feature is not enabled"
                })),
            );
        }
    };
    let result = trash_service.empty_trash(auth_user.id).await;

    match result {
        Ok(_) => {
            debug!("Trash emptied successfully");
            (
                StatusCode::OK,
                Json(json!({
                    "success": true,
                    "message": "Trash emptied successfully"
                })),
            )
        }
        Err(e) => {
            error!("Error emptying trash: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "Error emptying trash"
                })),
            )
        }
    }
}
