//! Domain types for the ReBAC authorization model.
//!
//! These types are storage-agnostic — they describe the relationship between
//! a subject (who), a resource (what), and a permission (action). The
//! `AuthorizationEngine` port consumes them and the `PgAclEngine` implementation
//! maps them to / from `storage.access_grants` rows.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use std::fmt;
use uuid::Uuid;

// ════════════════════════════════════════════════════════════════════════════
// Subject — who has the permission
// ════════════════════════════════════════════════════════════════════════════

/// A principal that can be granted permissions.
///
/// All variants carry a `Uuid` that uniquely identifies the subject within
/// its type's namespace.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Subject {
    /// A registered OxiCloud user (`auth.users.id`).
    User(Uuid),
    /// A user group (reserved for future use; no group CRUD in v1).
    Group(Uuid),
    /// An anonymous share token (`storage.shares.id`).
    Token(Uuid),
    /// A federated identity from another server — Open Cloud Mesh, external
    /// OIDC, etc. Refers to `auth.external_subjects.id` (future table).
    External(Uuid),
}

impl Subject {
    /// SQL discriminator string matching the `subject_type` CHECK constraint.
    pub fn type_str(&self) -> &'static str {
        match self {
            Subject::User(_) => "user",
            Subject::Group(_) => "group",
            Subject::Token(_) => "token",
            Subject::External(_) => "external",
        }
    }

    /// The raw UUID regardless of variant.
    pub fn id(&self) -> Uuid {
        match self {
            Subject::User(id) | Subject::Group(id) | Subject::Token(id) | Subject::External(id) => {
                *id
            }
        }
    }

    /// Reconstruct from a SQL row's `(subject_type, subject_id)` pair.
    pub fn from_parts(subject_type: &str, id: Uuid) -> Option<Self> {
        match subject_type {
            "user" => Some(Subject::User(id)),
            "group" => Some(Subject::Group(id)),
            "token" => Some(Subject::Token(id)),
            "external" => Some(Subject::External(id)),
            _ => None,
        }
    }
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", self.type_str(), self.id())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Resource — what the permission is on
// ════════════════════════════════════════════════════════════════════════════

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Resource {
    Folder(Uuid),
    File(Uuid),
    // Reserved for future use:
    // Calendar(Uuid),
    // Reserved for future use:
    // AddressBook(Uuid),
    // Reserved for future use:
    // Playlist(Uuid),
}

impl Resource {
    pub fn type_str(&self) -> &'static str {
        match self {
            Resource::Folder(_) => "folder",
            Resource::File(_) => "file",
            //Resource::Calendar(_) => "calendar",
            //Resource::AddressBook(_) => "adressbook",
            //Resource::Playlist(_) => "playlist",
        }
    }

    pub fn id(&self) -> Uuid {
        match self {
            Resource::Folder(id)
            | Resource::File(id)
            //| Resource::Calendar(id)
            //| Resource::AddressBook(id)
            //| Resource::Playlist(id)
            => *id,
        }
    }

    pub fn from_parts(resource_type: &str, id: Uuid) -> Option<Self> {
        match resource_type {
            "folder" => Some(Resource::Folder(id)),
            "file" => Some(Resource::File(id)),
            //"calendar" => Some(Resource::Calendar(id)),
            //"adressbook" => Some(Resource::AddressBook(id)),
            //"playlist" => Some(Resource::Playlist(id)),
            _ => None,
        }
    }
}

impl fmt::Display for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", self.type_str(), self.id())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Permission — what action is allowed
// ════════════════════════════════════════════════════════════════════════════

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Permission {
    /// View resource content / list folder contents.
    Read,
    /// Create child resources inside (only meaningful on folders).
    Create,
    /// Grant permissions to other subjects.
    Share,
    /// Add comments (reserved — comments feature not implemented yet).
    Comment,
    /// Delete the resource.
    Delete,
    /// Modify the resource (rename, move, edit content).
    Update,
}

impl Permission {
    /// Every permission, in a stable order. Used by `Role::expand()` and SQL
    /// `permission = ANY(...)` lookups.
    pub const ALL: [Permission; 6] = [
        Permission::Read,
        Permission::Create,
        Permission::Share,
        Permission::Comment,
        Permission::Delete,
        Permission::Update,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Permission::Read => "read",
            Permission::Create => "create",
            Permission::Share => "share",
            Permission::Comment => "comment",
            Permission::Delete => "delete",
            Permission::Update => "update",
        }
    }

    /// Parse a permission from its SQL discriminator string. Returns None
    /// for unknown values.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Permission::Read),
            "create" => Some(Permission::Create),
            "share" => Some(Permission::Share),
            "comment" => Some(Permission::Comment),
            "delete" => Some(Permission::Delete),
            "update" => Some(Permission::Update),
            _ => None,
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Grant — a row in storage.access_grants
// ════════════════════════════════════════════════════════════════════════════

#[derive(Clone, Debug)]
pub struct Grant {
    pub id: Uuid,
    pub subject: Subject,
    pub resource: Resource,
    pub permission: Permission,
    pub granted_by: Uuid,
    pub granted_at: chrono::DateTime<chrono::Utc>,
}

// ════════════════════════════════════════════════════════════════════════════
// ResourceKind — type-only discriminator (no id), used for filtering queries
// ════════════════════════════════════════════════════════════════════════════

/// Resource type without an id — used to filter paginated grant queries by
/// type. Mirrors the `resource_type` column values in `storage.access_grants`.
/// Add new variants here when new resource types are supported.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResourceKind {
    File,
    Folder,
    // Future: Calendar, AddressBook, Playlist, …
}

impl ResourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResourceKind::File => "file",
            ResourceKind::Folder => "folder",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "file" => Some(ResourceKind::File),
            "folder" => Some(ResourceKind::Folder),
            _ => None,
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// IncomingGrantSummary — aggregated across multiple permission rows
// ════════════════════════════════════════════════════════════════════════════

/// Multiple `access_grants` rows for the same `(subject, resource)` collapsed
/// into one record. Used by `list_incoming_resources_paged` to avoid sending
/// duplicate resource items to the caller.
#[derive(Debug, Clone)]
pub struct IncomingGrantSummary {
    pub resource_type: ResourceKind,
    pub resource_id: Uuid,
    /// All permissions held on this resource (aggregated).
    pub permissions: Vec<Permission>,
    /// Earliest `granted_at` across all permission rows.
    pub granted_at: chrono::DateTime<chrono::Utc>,
    /// Granter of the earliest grant.
    pub granted_by: Uuid,
}

// ════════════════════════════════════════════════════════════════════════════
// GrantCursor — opaque pagination cursor for list_incoming_resources_paged
// ════════════════════════════════════════════════════════════════════════════

/// Encodes the position of the last seen item in a cursor-paginated grant
/// listing. The encoding is opaque to API callers — only the backend
/// decodes it. Change the encoding algorithm in a major version bump.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GrantCursor {
    pub granted_at: chrono::DateTime<chrono::Utc>,
    pub resource_id: Uuid,
}

impl GrantCursor {
    /// Encode as a URL-safe base64 JSON string (no padding).
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        URL_SAFE_NO_PAD.encode(&json)
    }

    /// Decode from a URL-safe base64 JSON string. Returns `None` on any
    /// parse failure — callers treat a bad cursor as "start from the top".
    pub fn decode(s: &str) -> Option<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(s).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_roundtrip() {
        let id = Uuid::new_v4();
        let cases = [
            Subject::User(id),
            Subject::Group(id),
            Subject::Token(id),
            Subject::External(id),
        ];
        for s in cases {
            let back = Subject::from_parts(s.type_str(), s.id()).unwrap();
            assert_eq!(s, back);
        }
        assert!(Subject::from_parts("unknown", id).is_none());
    }

    #[test]
    fn resource_roundtrip() {
        let id = Uuid::new_v4();
        for r in [Resource::Folder(id), Resource::File(id)] {
            let back = Resource::from_parts(r.type_str(), r.id()).unwrap();
            assert_eq!(r, back);
        }
        assert!(Resource::from_parts("calendar", id).is_none());
    }

    #[test]
    fn permission_roundtrip() {
        for p in Permission::ALL {
            assert_eq!(Permission::parse(p.as_str()), Some(p));
        }
        assert!(Permission::parse("administrate").is_none());
    }
}
