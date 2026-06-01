//! External-identity service.
//!
//! Houses the lifecycle hook for grant-only external users — recipients
//! authenticating via magic-link, OIDC-only, or OCM federation rather than
//! a local password. Today the module ships only a **stubbed
//! `ExternalIdentityLifecycleHook`**: it's registered on the dispatcher
//! so the slot exists in DI, but every method is an explicit `Ok(())`
//! no-op. The magic-link PR sequence will fill in the bodies.
//!
//! # What the populated hook will do (forward reference)
//!
//! A future `auth.user_external_identity` side-table will store provenance
//! per external user:
//!
//! ```text
//! user_id           UUID PRIMARY KEY REFERENCES auth.users(id) ON DELETE CASCADE
//! source            TEXT NOT NULL CHECK (source IN ('magic_link','oidc','ocm'))
//! issuer            TEXT     -- OIDC iss URL or OCM partner FQDN
//! external_sub      TEXT     -- OIDC sub or OCM remote user id; NULL for magic_link
//! last_verified_at  TIMESTAMPTZ NOT NULL DEFAULT now()
//! UNIQUE (source, issuer, external_sub)
//! ```
//!
//! Then this hook will:
//!
//! | Event             | Action |
//! |-------------------|--------|
//! | `on_user_created` | If `user.is_external()`, INSERT a row into `auth.user_external_identity` with the source/issuer/sub captured from the create flow (magic-link bootstrap, OIDC JIT, OCM federation). |
//! | `on_user_login`   | If `user.is_external()`, `UPDATE … SET last_verified_at = NOW()` for the user's provenance row. Used by the GDPR sweeper to identify "external users we haven't heard from in 13 months". |
//! | `on_user_logout`  | `Ok(())` — provenance is connection-level, not session-level. |
//! | `on_user_deleted` | `Ok(())` — the FK CASCADE on `user_external_identity.user_id` handles row removal. |
//!
//! Today (PR 5): all four methods return `Ok(())` so the dispatcher
//! exercises the registration path without any side effect.

use async_trait::async_trait;

use crate::application::ports::user_lifecycle::{DeletionMode, LogoutReason, UserLifecycleHook};
use crate::common::errors::DomainError;
use crate::domain::entities::user::User;

/// **Stubbed for now.** Populates the future `auth.user_external_identity`
/// side-table when the magic-link / external-user flow ships. Registered
/// on the dispatcher today as a no-op so the magic-link PR doesn't need to
/// touch DI — it only fills in the hook body.
///
/// All four `UserLifecycleHook` methods are explicit `Ok(())` per the
/// "no defaults — every event acknowledged" convention.
pub struct ExternalIdentityLifecycleHook;

#[async_trait]
impl UserLifecycleHook for ExternalIdentityLifecycleHook {
    fn name(&self) -> &'static str {
        "external_identity"
    }

    async fn on_user_created(&self, _user: &User) -> Result<(), DomainError> {
        // STUB: magic-link / OIDC JIT / OCM bootstrap PR will INSERT the
        // provenance row here when `user.is_external()`.
        Ok(())
    }

    async fn on_user_login(&self, _user: &User) -> Result<(), DomainError> {
        // STUB: magic-link PR will UPDATE `last_verified_at` here so the
        // GDPR sweeper can identify dormant external users.
        Ok(())
    }

    async fn on_user_logout(&self, _user: &User, _reason: LogoutReason) -> Result<(), DomainError> {
        // Provenance is connection-level, not session-level — no work
        // to do on logout even in the populated future version.
        Ok(())
    }

    async fn on_user_deleted(
        &self,
        _user: &User,
        _mode: DeletionMode,
        _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), DomainError> {
        // FK CASCADE on `auth.user_external_identity.user_id` will
        // handle row removal automatically — no work needed here even
        // in the populated future version.
        Ok(())
    }
}
