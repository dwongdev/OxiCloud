//! Authorization port — the trait every service depends on for permission
//! decisions. Implementations: `PgAclEngine` (v1 default), `OpenFgaEngine`
//! (future). A `CachedAuthorizationEngine` decorator over either is planned
//! as a future optimization.
//!
//! Architectural rule (see CLAUDE.md):
//! **AuthZ is enforced exclusively in the application service layer.**
//! Handlers authenticate the caller and pass `caller_id` to the service;
//! they never call this trait directly.

use uuid::Uuid;

use crate::common::errors::DomainError;
use crate::domain::services::authorization::{
    Grant, GrantCursor, IncomingGrantSummary, Permission, Resource, ResourceKind, Subject,
};

pub trait AuthorizationEngine: Send + Sync + 'static {
    /// Returns true if `subject` has `permission` on `resource`, considering
    /// owner short-circuit AND cascading from folder ancestors.
    ///
    /// `check` never errors for "permission denied" — that's a `false` return.
    /// `Err` is reserved for infrastructure failures (DB down, etc.).
    async fn check(
        &self,
        subject: Subject,
        permission: Permission,
        resource: Resource,
    ) -> Result<bool, DomainError>;

    /// Convenience wrapper around `check`: returns `Ok(())` when allowed and
    /// `DomainError::not_found` when denied (anti-enumeration — same error as
    /// "resource doesn't exist" so attackers can't probe IDs by error shape).
    async fn require(
        &self,
        subject: Subject,
        permission: Permission,
        resource: Resource,
    ) -> Result<(), DomainError> {
        if self.check(subject, permission, resource).await? {
            tracing::debug!(
                "👮🏻‍♂️ perms: ✔ Subject '{}' has permission to '{}' on resource '{}'",
                subject,
                permission,
                resource
            );
            Ok(())
        } else {
            let (kind, id) = match resource {
                Resource::Folder(id) => ("Folder", id),
                Resource::File(id) => ("File", id),
            };
            // log it for audit
            tracing::info!(
                "👮🏻‍♂️ perms: ⛔ Subject '{}' hasn't permission to '{}' on resource '{}'",
                subject,
                permission,
                resource
            );
            Err(DomainError::not_found(kind, id.to_string()))
        }
    }

    /// Resources explicitly granted to `subject`. Direct grants only — no
    /// cascade expansion. Used by `GET /api/grants/incoming`.
    async fn list_incoming_grants(
        &self,
        subject: Subject,
        permission_filter: Option<Permission>,
    ) -> Result<Vec<Grant>, DomainError>;

    /// Cursor-paginated list of resources explicitly granted to `subject`,
    /// optionally filtered by resource kind. Multiple permission rows for the
    /// same resource are collapsed into one `IncomingGrantSummary`.
    ///
    /// Ordered by `MIN(granted_at) DESC, resource_id DESC` — stable across
    /// concurrent inserts because the cursor encodes both fields.
    ///
    /// Pass `kinds = &[]` to return all resource kinds.
    /// Returns `(summaries, next_cursor)` — `next_cursor` is `None` when the
    /// last page has been reached.
    async fn list_incoming_resources_paged(
        &self,
        subject: Subject,
        kinds: &[ResourceKind],
        limit: u32,
        cursor: Option<GrantCursor>,
    ) -> Result<(Vec<IncomingGrantSummary>, Option<GrantCursor>), DomainError>;

    /// All grants on a specific resource (for "Manage sharing" UI). Caller
    /// must verify the caller has `Share` on the resource before invoking.
    async fn list_grants_on_resource(&self, resource: Resource) -> Result<Vec<Grant>, DomainError>;

    /// Grants Outgoing — grants created by `granted_by`. Used by
    /// `GET /api/grants/outgoing` ("things I've shared with others").
    async fn list_outgoing_grants(&self, granted_by: Uuid) -> Result<Vec<Grant>, DomainError>;

    /// Create a grant. Idempotent — duplicates are absorbed by the UNIQUE
    /// constraint and the existing row is returned.
    async fn grant(
        &self,
        granted_by: Uuid,
        subject: Subject,
        permission: Permission,
        resource: Resource,
    ) -> Result<Grant, DomainError>;

    /// Revoke a specific grant by its UUID. Returns `Ok(())` whether or not
    /// the row existed (idempotent revoke).
    async fn revoke(&self, grant_id: Uuid) -> Result<(), DomainError>;

    /// Removes every grant whose `resource` matches. Called by lifecycle
    /// hooks when a resource is permanently deleted. Returns the count of
    /// rows removed.
    async fn revoke_all_for_resource(&self, resource: Resource) -> Result<usize, DomainError>;

    /// Removes every grant whose `subject` matches. Called when a user/token
    /// /group is deleted. Returns the count of rows removed.
    async fn revoke_all_for_subject(&self, subject: Subject) -> Result<usize, DomainError>;
}
