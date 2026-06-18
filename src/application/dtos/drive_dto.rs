//! DTOs for the `/api/drives` endpoint surface.
//!
//! D0 surfaces only the read-only list. Mutating endpoints
//! (`POST /api/drives` for shared-drive creation, `PATCH` for rename /
//! policy edits, membership APIs) land in D2/D3.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::domain::entities::drive::{Drive, DriveKind};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DriveKindDto {
    Personal,
    Shared,
}

impl From<DriveKind> for DriveKindDto {
    fn from(k: DriveKind) -> Self {
        match k {
            DriveKind::Personal => DriveKindDto::Personal,
            DriveKind::Shared => DriveKindDto::Shared,
        }
    }
}

/// One row in `GET /api/drives` — a drive the caller can read.
///
/// `default_for_user` is `Some(<caller_id>)` for the caller's default
/// Personal drive and `None` otherwise. The picker UI uses this to put
/// the default at the top of the list and mark it as "your home".
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DriveDto {
    pub id: Uuid,
    pub name: String,
    pub kind: DriveKindDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_for_user: Option<Uuid>,
    /// Storage cap in bytes. `None` means "no quota" (admin override /
    /// future system drives).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota_bytes: Option<i64>,
    /// Running total of bytes consumed. Maintained incrementally in D4;
    /// on D0 this reflects the backfilled baseline.
    pub used_bytes: i64,
    /// Capability-flag bag — clients render UI affordances based on
    /// known keys (`forbid_public_links`, `include_in_photo_index`,
    /// `forbid_music_index`, …). Unknown keys preserved verbatim.
    pub policies: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<Drive> for DriveDto {
    fn from(d: Drive) -> Self {
        Self {
            id: d.id,
            name: d.name,
            kind: d.kind.into(),
            default_for_user: d.default_for_user,
            quota_bytes: d.quota_bytes,
            used_bytes: d.used_bytes,
            policies: d.policies,
            created_at: d.created_at,
            updated_at: d.updated_at,
        }
    }
}
