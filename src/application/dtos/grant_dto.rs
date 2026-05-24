//! DTOs for the ReBAC `/api/grants` REST endpoints.
//!
//! The wire shapes are intentionally separate from the domain types
//! (`Subject`, `Resource`, `Permission`, `Grant`) so that domain stays
//! storage-agnostic and DTOs can evolve with the HTTP contract.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::application::dtos::file_dto::FileDto;
use crate::application::dtos::folder_dto::FolderDto;
use crate::domain::services::authorization::{Grant, Permission, Resource, Subject};

// ════════════════════════════════════════════════════════════════════════════
// Subject / Resource / Permission DTOs
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SubjectTypeDto {
    User,
    Group,
    Token,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubjectDto {
    #[serde(rename = "type")]
    pub kind: SubjectTypeDto,
    pub id: Uuid,
}

impl From<SubjectDto> for Subject {
    fn from(dto: SubjectDto) -> Self {
        match dto.kind {
            SubjectTypeDto::User => Subject::User(dto.id),
            SubjectTypeDto::Group => Subject::Group(dto.id),
            SubjectTypeDto::Token => Subject::Token(dto.id),
            SubjectTypeDto::External => Subject::External(dto.id),
        }
    }
}

impl From<Subject> for SubjectDto {
    fn from(s: Subject) -> Self {
        let (kind, id) = match s {
            Subject::User(id) => (SubjectTypeDto::User, id),
            Subject::Group(id) => (SubjectTypeDto::Group, id),
            Subject::Token(id) => (SubjectTypeDto::Token, id),
            Subject::External(id) => (SubjectTypeDto::External, id),
        };
        SubjectDto { kind, id }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ResourceTypeDto {
    Folder,
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResourceDto {
    #[serde(rename = "type")]
    pub kind: ResourceTypeDto,
    pub id: Uuid,
}

impl From<ResourceDto> for Resource {
    fn from(dto: ResourceDto) -> Self {
        match dto.kind {
            ResourceTypeDto::Folder => Resource::Folder(dto.id),
            ResourceTypeDto::File => Resource::File(dto.id),
        }
    }
}

impl From<Resource> for ResourceDto {
    fn from(r: Resource) -> Self {
        let (kind, id) = match r {
            Resource::Folder(id) => (ResourceTypeDto::Folder, id),
            Resource::File(id) => (ResourceTypeDto::File, id),
        };
        ResourceDto { kind, id }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PermissionDto {
    Read,
    Create,
    Share,
    Comment,
    Delete,
    Update,
}

impl From<PermissionDto> for Permission {
    fn from(p: PermissionDto) -> Self {
        match p {
            PermissionDto::Read => Permission::Read,
            PermissionDto::Create => Permission::Create,
            PermissionDto::Share => Permission::Share,
            PermissionDto::Comment => Permission::Comment,
            PermissionDto::Delete => Permission::Delete,
            PermissionDto::Update => Permission::Update,
        }
    }
}

impl From<Permission> for PermissionDto {
    fn from(p: Permission) -> Self {
        match p {
            Permission::Read => PermissionDto::Read,
            Permission::Create => PermissionDto::Create,
            Permission::Share => PermissionDto::Share,
            Permission::Comment => PermissionDto::Comment,
            Permission::Delete => PermissionDto::Delete,
            Permission::Update => PermissionDto::Update,
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Roles (DTO-layer sugar)
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    //Commenter,
    Editor,
    //Manager,
    Admin,
}

impl Role {
    /// Expands a role into its constituent raw permissions. Storage and
    /// engine know nothing about roles — the server normalizes here before
    /// writing rows.
    pub fn expand(self) -> &'static [Permission] {
        match self {
            Role::Viewer => &[Permission::Read],
            /* reserved for future
            Role::Commenter => &[Permission::Read, Permission::Comment],
            */
            Role::Editor => &[
                Permission::Read,
                Permission::Comment,
                Permission::Create,
                Permission::Update,
            ],
            /* reserved for future
            Role::Manager => &[
                Permission::Read,
                Permission::Comment,
                Permission::Create,
                Permission::Update,
                Permission::Share,
            ],
            */
            Role::Admin => &[
                Permission::Read,
                Permission::Comment,
                Permission::Create,
                Permission::Update,
                Permission::Share,
                Permission::Delete,
            ],
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Request DTOs
// ════════════════════════════════════════════════════════════════════════════

/// `POST /api/grants` — accepts either `permissions` (explicit) or `role`.
/// Server-side validation requires exactly one of the two to be present.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateGrantDto {
    pub subject: SubjectDto,
    pub resource: ResourceDto,
    #[serde(default)]
    pub permissions: Option<Vec<PermissionDto>>,
    #[serde(default)]
    pub role: Option<Role>,
}

/// `PUT /api/grants/role` — reconcile a subject's role on a resource.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateRoleDto {
    pub subject: SubjectDto,
    pub resource: ResourceDto,
    pub role: Role,
}

// ════════════════════════════════════════════════════════════════════════════
// Response DTOs
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GrantDto {
    pub id: Uuid,
    pub subject: SubjectDto,
    pub resource: ResourceDto,
    pub permission: PermissionDto,
    pub granted_by: Uuid,
    pub granted_at: chrono::DateTime<chrono::Utc>,
}

impl From<Grant> for GrantDto {
    fn from(g: Grant) -> Self {
        Self {
            id: g.id,
            subject: g.subject.into(),
            resource: g.resource.into(),
            permission: g.permission.into(),
            granted_by: g.granted_by,
            granted_at: g.granted_at,
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Shared-with-me DTOs  (GET /api/grants/incoming/resources)
// ════════════════════════════════════════════════════════════════════════════

/// Query parameters for `GET /api/grants/incoming/resources`.
#[derive(Debug, Deserialize, IntoParams)]
pub struct SharedWithMeQuery {
    /// Maximum number of items to return (1–200, default 50).
    #[serde(default = "shared_with_me_default_limit")]
    pub limit: u32,
    /// Comma-separated resource types to include, e.g. `file,folder`.
    /// Omit to return all known types.
    pub resource_types: Option<String>,
    /// Opaque cursor returned by a previous call. Omit to start from the
    /// most-recently-granted item.
    pub cursor: Option<String>,
}

fn shared_with_me_default_limit() -> u32 {
    50
}

/// One item in the shared-with-me list. Exactly one of `file` / `folder` is
/// populated, indicated by `resource_type`. Additional optional fields for
/// future resource types (playlist, addressbook, …) will be added here.
#[derive(Debug, Serialize, ToSchema)]
pub struct SharedWithMeItemDto {
    pub resource_type: ResourceTypeDto,
    /// All permissions the caller holds on this resource (aggregated).
    pub permissions: Vec<PermissionDto>,
    /// Earliest grant date for this resource.
    pub granted_at: chrono::DateTime<chrono::Utc>,
    /// UUID of the user who created the (earliest) grant.
    pub granted_by: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FileDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<FolderDto>,
}

/// Response for `GET /api/grants/incoming/resources`.
#[derive(Debug, Serialize, ToSchema)]
pub struct SharedWithMeDto {
    pub items: Vec<SharedWithMeItemDto>,
    /// Opaque cursor for the next page. Absent when the last page is reached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
