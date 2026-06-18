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

/// A drive paired with the display name from its root folder.
///
/// `storage.drives` has no `name` column under the D0 design
/// (docs/plan/drive.md §3) — the display name lives on
/// `storage.folders.name` of the row pointed at by `drive.root_folder_id`.
/// Read paths join the two tables and hand callers this view-model so the
/// API surface can continue to expose a single "drive with name" shape
/// without a follow-up query per drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveWithRootName {
    pub drive: Drive,
    /// The drive's display name. Sourced from `storage.folders.name`
    /// of the root folder via JOIN at read time.
    pub root_folder_name: String,
}

#[async_trait::async_trait]
pub trait DriveRepository: Send + Sync + 'static {
    /// Atomically create a personal drive together with its root folder
    /// and the owner role_grant — all four DB writes in a single SQL
    /// statement (docs/plan/drive.md §3 "Atomic creation"). The
    /// statement runs as its own implicit transaction in autocommit mode
    /// so a server crash mid-statement leaves no half-row state.
    ///
    /// The root folder is created with name `"Personal"` (the canonical
    /// default) and `parent_id IS NULL`. The drive's `root_folder_id`
    /// is wired to point at it before the statement commits.
    ///
    /// Returns `DefaultDriveAlreadyExists` when the owner already has a
    /// default drive — relies on the partial UNIQUE index on
    /// `default_for_user`.
    async fn create_personal_drive_atomic(
        &self,
        owner_id: Uuid,
        quota_bytes: Option<i64>,
    ) -> Result<DriveWithRootName, DriveRepositoryError>;

    /// Fetch a drive by id together with its display name. `NotFound`
    /// when no row matches.
    async fn get_by_id(&self, id: Uuid) -> Result<DriveWithRootName, DriveRepositoryError>;

    /// Return the caller's default personal drive paired with its
    /// display name, or `NotFound` if they don't have one (e.g.
    /// external users; users created before the lifecycle hook fired).
    /// Drives the Photos timeline scope, the `/api/recent/*` scope, and
    /// D1's redirect-from-`/`.
    async fn find_default_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<DriveWithRootName, DriveRepositoryError>;

    /// List drives the caller can read, resolved via `role_grants` for
    /// `resource_type='drive'`. The caller's group memberships are
    /// expanded by the engine's `subject_match_set`; that expanded set
    /// is what this method's `subject_ids` argument carries.
    ///
    /// Returns rows in a stable order: default drive first (if any),
    /// then by display name. The `/api/drives` handler relies on that
    /// order for the picker UI without a follow-up sort.
    async fn list_for_subjects(
        &self,
        subject_types: &[&str],
        subject_ids: &[Uuid],
    ) -> Result<Vec<DriveWithRootName>, DriveRepositoryError>;
}

/// Convenience: convert the canonical kind discriminator from its SQL
/// form into the typed enum. Mirrored on the entity for symmetry.
impl DriveKind {
    pub fn from_sql(s: &str) -> Result<Self, DriveRepositoryError> {
        DriveKind::parse(s).ok_or_else(|| DriveRepositoryError::InvalidKind(s.to_owned()))
    }
}
