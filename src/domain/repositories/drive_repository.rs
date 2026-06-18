//! Repository for [`Drive`] entities backed by `storage.drives`.
//!
//! Drives have no separate membership table — owner/editor/viewer
//! membership lives in `storage.role_grants` with
//! `resource_type='drive'`. That means **listing the drives a user can
//! reach goes through the role-grant query, not through this
//! repository**. This repo handles:
//!
//!   * Creating a drive (used by the user-creation lifecycle hook and
//!     by D3's shared-drive flow).
//!   * Looking up a single drive by id (used by the engine's owner_of /
//!     check paths, by `/api/drives/{id}`, and by the drive picker).
//!   * Finding the caller's default drive (used by the Photos / Music
//!     endpoints and by D1's redirect-from-`/` logic).
//!
//! Membership-flavoured queries (e.g. "list every drive user X can
//! read") live in `DriveListingService` (post-D0) which reads
//! `role_grants` and resolves the matching drive rows here.

use thiserror::Error;
use uuid::Uuid;

use crate::domain::entities::drive::{Drive, DriveKind};

#[derive(Debug, Error)]
pub enum DriveRepositoryError {
    #[error("Drive not found: {0}")]
    NotFound(String),
    /// A user already has a default drive set — partial unique index on
    /// `default_for_user` rejects a second one. Surfaces the constraint
    /// explicitly so the lifecycle hook can no-op idempotently.
    #[error("User already has a default drive: {0}")]
    DefaultDriveAlreadyExists(String),
    #[error("Invalid drive kind: {0}")]
    InvalidKind(String),
    #[error("Storage error: {0}")]
    StorageError(String),
}

/// Input parameters for creating a new personal drive.
///
/// Shared drives land in D3 with their own creation surface
/// (`create_shared_drive`). For now D0 only mints personal drives —
/// either as the default for a fresh user (via the lifecycle hook) or
/// as a secondary promoted by the M2 backfill.
#[derive(Debug, Clone)]
pub struct CreatePersonalDriveInput {
    /// Display name. The lifecycle hook passes `"Personal"`; the M2
    /// backfill carries over the original sibling-root folder name for
    /// secondaries.
    pub name: String,
    /// The owner. For personal drives the owner is exactly one user.
    pub owner_id: Uuid,
    /// `true` when this is the user's default drive (sets the partial-
    /// unique `default_for_user` column). `false` for secondaries.
    pub is_default: bool,
    /// Initial storage quota in bytes. `None` defers to admin policy
    /// (typically copied from `auth.users.storage_quota_bytes` at the
    /// call site).
    pub quota_bytes: Option<i64>,
}

#[async_trait::async_trait]
pub trait DriveRepository: Send + Sync + 'static {
    /// Insert a personal drive row. The caller is responsible for
    /// inserting the matching owner row in `storage.role_grants` in the
    /// same transaction (the lifecycle hook handles this; M2's backfill
    /// did it directly in SQL).
    ///
    /// Returns `DefaultDriveAlreadyExists` when `is_default=true` and the
    /// owner already has a default drive — relies on the partial UNIQUE
    /// index on `default_for_user`.
    async fn create_personal(
        &self,
        input: CreatePersonalDriveInput,
    ) -> Result<Drive, DriveRepositoryError>;

    /// Fetch a drive by id. `NotFound` when no row matches.
    async fn get_by_id(&self, id: Uuid) -> Result<Drive, DriveRepositoryError>;

    /// Return the caller's default personal drive, or `NotFound` if they
    /// don't have one (e.g. external users; users created before the
    /// lifecycle hook fired). Drives the Photos timeline scope, the
    /// `/api/recent/*` scope, and D1's redirect-from-`/`.
    async fn find_default_for_user(&self, user_id: Uuid) -> Result<Drive, DriveRepositoryError>;

    /// List drives the caller can read, resolved via `role_grants` for
    /// `resource_type='drive'`. The caller's group memberships are
    /// expanded by the engine's `subject_match_set`; that expanded set
    /// is what this method's `subject_ids` argument carries.
    ///
    /// Returns rows in a stable order: default drive first (if any),
    /// then by name. The `/api/drives` handler relies on that order for
    /// the picker UI without a follow-up sort.
    async fn list_for_subjects(
        &self,
        subject_types: &[&str],
        subject_ids: &[Uuid],
    ) -> Result<Vec<Drive>, DriveRepositoryError>;
}

/// Convenience: convert the canonical kind discriminator from its SQL
/// form into the typed enum. Mirrored on the entity for symmetry.
impl DriveKind {
    pub fn from_sql(s: &str) -> Result<Self, DriveRepositoryError> {
        DriveKind::parse(s).ok_or_else(|| DriveRepositoryError::InvalidKind(s.to_owned()))
    }
}
