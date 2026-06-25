//! Drive — the top-level container that owns a tree of folders/files.
//!
//! Drives replaced the per-user `My Folder - <username>` wrapper at D0.
//! Every folder and file row carries a `drive_id` (added by D0's
//! migration); a drive is the natural unit of quota, sharing, and
//! lifecycle. Membership is expressed through `storage.role_grants` rows
//! with `resource_type='drive'` — there is no separate `drive_members`
//! table.
//!
//! ## Kinds
//!
//! Two kinds today; the discriminant is the `kind` column with a CHECK
//! constraint.
//!
//! - **`personal`** — single-user, single-owner. The owner is captured
//!   by `default_for_user` (for the default Personal drive) or by an
//!   Owner role_grant on a secondary personal drive. Personal drives
//!   refuse `add_member`, `remove_member`, and `delete_drive` (when
//!   it's the user's only or default drive). A user can have multiple
//!   personal drives — one is marked default (`default_for_user =
//!   <uid>`), the others are secondaries (`default_for_user = NULL`,
//!   one Owner row in role_grants pinning them to the same user).
//!
//! - **`shared`** — multi-member, group-aware, full role roster
//!   (viewer / commenter / contributor / editor / owner). Members
//!   come from role_grants; group subjects expand transitively via
//!   the existing `subject_groups` machinery. Last-owner protection
//!   applies on member removal and drive deletion. Quota is set by
//!   the drive owner (or admin); `used_bytes` tracks consumption.
//!
//! Future kinds (e.g. `system` for built-in scratch space) drop in by
//! extending the CHECK + the `DriveKind` enum.
//!
//! ## Policies
//!
//! `policies` is a JSONB bag carrying feature flags / capability toggles
//! that drive owners can flip without a schema change. Known keys live in
//! `docs/plan/drive.md` §8 and §15 (e.g. `forbid_public_links`,
//! `include_in_photo_index`, `forbid_music_index`). Unknown keys are
//! preserved by the application — the schema is intentionally permissive
//! so future capability flags can land without a migration.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Drive kind discriminant. Mirrors the `storage.drives.kind` CHECK
/// constraint values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DriveKind {
    /// Single-owner storage compartment. Cannot have members added or
    /// removed via the membership API; the owner is fixed for the drive's
    /// lifetime.
    Personal,
    /// Multi-member drive supporting the full role roster. Membership is
    /// open to admin/owner-driven changes through the membership API.
    Shared,
}

impl DriveKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DriveKind::Personal => "personal",
            DriveKind::Shared => "shared",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "personal" => Some(DriveKind::Personal),
            "shared" => Some(DriveKind::Shared),
            _ => None,
        }
    }
}

/// Domain entity for a row in `storage.drives`.
///
/// Drives are pure metadata under the D0 design (docs/plan/drive.md §3):
/// no `name` column — the display name lives on the root folder pointed
/// at by `root_folder_id`. Code that needs the name pairs this struct
/// with a JOIN through `storage.folders`; see the repository's
/// `DriveWithRootName` view-model.
///
/// Field-level constraints are enforced at the SQL layer (CHECK on
/// `kind`, partial UNIQUE on `default_for_user`). The struct mirrors
/// the column set 1:1; behaviour beyond field access lives in
/// `DriveRepository` and `DriveService` (post-D0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drive {
    /// Stable identifier. Generated server-side at creation.
    pub id: Uuid,
    /// Discriminant — see [`DriveKind`].
    pub kind: DriveKind,
    /// Set iff this is the user's default personal drive (UNIQUE in SQL
    /// via a partial index `WHERE default_for_user IS NOT NULL`). NULL
    /// on shared drives and on secondary personal drives.
    pub default_for_user: Option<Uuid>,
    /// The drive's mount-point folder. The column is NULLable in SQL
    /// only because the atomic creation CTE writes it mid-statement
    /// (a column-level `NOT NULL` would refuse the initial drive INSERT
    /// — see docs/plan/drive.md §3). After any successful creation path,
    /// this is populated; code reading `Drive` may treat it as `Uuid`,
    /// not `Option<Uuid>`. A NULL at read time is a data-invariant bug.
    pub root_folder_id: Uuid,
    /// Soft cap on this drive's storage usage, in bytes. `None` means
    /// "no quota" (rare; reserved for admin overrides). The default
    /// initial quota for a fresh personal drive is taken from the
    /// owner's `auth.users.storage_quota_bytes` at creation time.
    /// **Mutation is OxiCloud-admin only** (docs/plan/drive.md §7) —
    /// not in the drive `owner` role bundle.
    pub quota_bytes: Option<i64>,
    /// Running total of bytes consumed. Maintained incrementally by
    /// upload/delete paths in D4; on D0 still reflects the pre-Drive
    /// per-user counters via the backfill.
    pub used_bytes: i64,
    /// Capability flags / feature toggles. Extensible JSONB — see
    /// `docs/plan/drive.md` §8 and §15 for the known keys.
    pub policies: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl Drive {
    /// `true` for the user's default personal drive (the only drive for
    /// which `default_for_user` is set to that user's id).
    pub fn is_default_for(&self, user_id: Uuid) -> bool {
        self.default_for_user == Some(user_id)
    }

    /// `true` if this drive is a personal drive of any kind (default or
    /// secondary). Encapsulates the kind check at the call site.
    pub fn is_personal(&self) -> bool {
        matches!(self.kind, DriveKind::Personal)
    }

    /// Typed view of `policies` for enforcement code. Lenient deserialise:
    /// unknown keys are preserved on disk (the column stays the canonical
    /// JSONB bag) but ignored here, missing keys default to `false`.
    /// See `docs/plan/drive.md` §8.
    pub fn typed_policies(&self) -> DrivePolicies {
        DrivePolicies::from_value(&self.policies)
    }
}

/// Typed mirror of the `policies` JSONB. Five known keys; the JSONB column
/// remains the source of truth and may carry unknown keys verbatim — this
/// struct is a read view for enforcement and a write view for the policy
/// PATCH endpoint. Every field defaults to `false` (everything allowed)
/// so a freshly-created drive doesn't need a populated policy bag.
///
/// See `docs/plan/drive.md` §8 for the enforcement matrix
/// (which callsite each key is checked at).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct DrivePolicies {
    /// Disables per-resource grants on resources in this drive. Drive-level
    /// membership (Owner/Editor/Viewer) still works. Enforced at
    /// `grant_handler::create_grant`.
    pub forbid_sharing: bool,
    /// Blocks grants whose subject has `users.is_external = true`. Enforced
    /// at `magic_link_invite_service::resolve_or_create_recipient` and
    /// `grant_handler::create_grant`.
    pub forbid_external_sharing: bool,
    /// Blocks anonymous-link (token-share) creation on resources in this
    /// drive. Enforced at `share_service::create_shared_link`.
    pub forbid_public_links: bool,
    /// Blocks MOVE when `src.drive_id != dst.drive_id`. Enforced at the
    /// move endpoints. Lands paired with D6's cross-drive move work.
    pub forbid_cross_drive_move: bool,
}

impl DrivePolicies {
    /// Parse from the raw JSONB. Lenient — unknown keys are dropped from
    /// the typed view but remain in the source `serde_json::Value`. A
    /// malformed bag (e.g. wrong type) falls back to the all-false default
    /// rather than refusing the read; enforcement code never panics on
    /// existing data.
    pub fn from_value(value: &serde_json::Value) -> Self {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }
}
