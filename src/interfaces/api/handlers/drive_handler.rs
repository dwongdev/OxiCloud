//! `GET /api/drives` — list every drive the caller can read.
//!
//! D0 ships the read-only listing; D2 adds shared-drive membership
//! mutations (`POST/DELETE/PUT /api/drives/{id}/members`), D3 adds the
//! create-shared-drive flow, etc.
//!
//! The handler resolves the caller's expanded subject set through the
//! engine (so group-mediated drive grants surface — the foundation for
//! D2/D3) and asks the `DriveRepository` for every drive that set can
//! read. Authorization is purely the subject-expansion step: no
//! `require(...)` call here, because "your accessible drives" is a
//! listing query, not a permission decision on a specific drive.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use tracing::error;

use crate::application::dtos::drive_dto::DriveDto;
use crate::common::di::AppState;
use crate::domain::repositories::drive_repository::DriveRepository;
use crate::domain::services::authorization::Subject;
use crate::interfaces::errors::AppError;
use crate::interfaces::middleware::auth::AuthUser;

#[utoipa::path(
    get,
    path = "/api/drives",
    responses(
        (status = 200, description = "Drives the caller can read", body = Vec<DriveDto>),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearerAuth" = [])),
    tag = "drives"
)]
pub async fn list_drives(
    State(state): State<Arc<AppState>>,
    auth_user: AuthUser,
) -> impl IntoResponse {
    let caller_id = auth_user.id;

    // Expand the caller's `Subject::User` into the `(types, ids)` pair
    // that includes every group the user transitively belongs to. The
    // engine caches this expansion in its Moka cache; if the caller
    // just ran a permission check, this is a hit.
    let (subject_types, subject_ids) = match state
        .authorization
        .expand_subject_for_listing(Subject::User(caller_id))
        .await
    {
        Ok(pair) => pair,
        Err(e) => {
            error!("list_drives: subject expansion failed: {e}");
            return AppError::from(e).into_response();
        }
    };

    match state
        .drive_repo
        .list_for_subjects(&subject_types, &subject_ids)
        .await
    {
        Ok(drives) => {
            let dtos: Vec<DriveDto> = drives.into_iter().map(DriveDto::from).collect();
            (StatusCode::OK, Json(dtos)).into_response()
        }
        Err(e) => {
            error!("list_drives: repo lookup failed: {e}");
            AppError::internal_error(format!("Failed to list drives: {e}")).into_response()
        }
    }
}
