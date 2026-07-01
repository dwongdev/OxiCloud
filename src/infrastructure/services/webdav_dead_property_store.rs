//! PostgreSQL-backed dead property store for WebDAV PROPPATCH / PROPFIND compliance.
//!
//! RFC 4918 §4.2 defines "dead properties" as those stored verbatim by the
//! server without interpreting their value. Properties are persisted to
//! `storage.webdav_dead_properties` and survive server restarts.
//!
//! Keying contract (after migration 20260830000001): the row is keyed by
//! the underlying resource id — exactly one of `folder_id` / `file_id` is
//! set — not by the resource's current path. Three consequences:
//!
//!   * Every delete code path (REST, WebDAV, NextCloud DAV, trash empty,
//!     folder cascade) reaps dead-property rows for free via FK
//!     `ON DELETE CASCADE`. The store has no `remove_resource()` method
//!     because it isn't needed: deleting the file/folder row reaps the
//!     attached dead properties as a database invariant.
//!   * MOVE / RENAME never changes the resource id, so dead properties
//!     follow the resource without any store-side bookkeeping. The store
//!     has no `rename_resource()` method for the same reason.
//!   * Dead properties are RESOURCE state (RFC 4918 §4.2), not user
//!     state. Two users on a shared drive PROPFIND'ing the same resource
//!     see the same dead properties. The `user_id` scope key from the
//!     pre-rekey schema is gone; user-delete cleanup happens
//!     transitively through `auth.users` → `storage.{folders,files}` →
//!     this table.
//!
//! Queries use `sqlx::query()` (runtime-bound) rather than `sqlx::query!()`
//! to keep fresh checkouts compilable without a DB connection — the
//! codebase's standing convention.
//!
//! COPY semantics (RFC 4918 §8.8 — dead properties MUST be duplicated)
//! are NOT handled here. The COPY handler is responsible for explicitly
//! reading the source's dead properties via `get_all()` and writing them
//! against the new resource id via `set()`. Not done in this migration —
//! it was not handled by the path-based store either, so this is a
//! parity decision, not a regression.

use std::sync::Arc;

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::application::adapters::webdav_adapter::QualifiedName;
use crate::domain::errors::DomainError;

/// Polymorphic reference to the resource a dead property hangs off.
///
/// Exactly one variant — folder or file — is ever stored in a single
/// row. The CHECK constraint
/// `(folder_id IS NULL) <> (file_id IS NULL)` enforces this at the
/// database level so the application layer cannot accidentally write a
/// row that's both or neither.
#[derive(Clone, Copy, Debug)]
pub enum ResourceRef {
    Folder(Uuid),
    File(Uuid),
}

pub struct DeadPropertyStore {
    pool: Arc<PgPool>,
}

impl DeadPropertyStore {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// Upsert a dead property. `value = None` means an empty XML element.
    ///
    /// The two SQL branches are deliberately kept separate so each
    /// ON CONFLICT clause can target the matching partial unique
    /// index (`idx_webdav_dead_props_folder_unique` /
    /// `idx_webdav_dead_props_file_unique`). A combined upsert would
    /// require a non-partial unique index that treats NULL as
    /// distinct, which doesn't match the (folder XOR file) shape.
    pub async fn set(
        &self,
        r: ResourceRef,
        name: QualifiedName,
        value: Option<String>,
    ) -> Result<(), DomainError> {
        match r {
            ResourceRef::Folder(folder_id) => {
                sqlx::query(
                    r#"
                    INSERT INTO storage.webdav_dead_properties
                        (folder_id, namespace, local_name, value)
                    VALUES ($1, $2, $3, $4)
                    ON CONFLICT (folder_id, namespace, local_name)
                        WHERE folder_id IS NOT NULL
                    DO UPDATE SET value = EXCLUDED.value, updated_at = CURRENT_TIMESTAMP
                    "#,
                )
                .bind(folder_id)
                .bind(&name.namespace)
                .bind(&name.name)
                .bind(&value)
                .execute(&*self.pool)
                .await
                .map_err(|e| {
                    DomainError::internal_error("DeadPropertyStore", format!("set folder: {e}"))
                })?;
            }
            ResourceRef::File(file_id) => {
                sqlx::query(
                    r#"
                    INSERT INTO storage.webdav_dead_properties
                        (file_id, namespace, local_name, value)
                    VALUES ($1, $2, $3, $4)
                    ON CONFLICT (file_id, namespace, local_name)
                        WHERE file_id IS NOT NULL
                    DO UPDATE SET value = EXCLUDED.value, updated_at = CURRENT_TIMESTAMP
                    "#,
                )
                .bind(file_id)
                .bind(&name.namespace)
                .bind(&name.name)
                .bind(&value)
                .execute(&*self.pool)
                .await
                .map_err(|e| {
                    DomainError::internal_error("DeadPropertyStore", format!("set file: {e}"))
                })?;
            }
        }
        Ok(())
    }

    /// Delete a specific dead property. No-op if not present.
    pub async fn remove(&self, r: ResourceRef, name: &QualifiedName) -> Result<(), DomainError> {
        let (folder_id, file_id) = split_ref(r);
        sqlx::query(
            "DELETE FROM storage.webdav_dead_properties
              WHERE folder_id IS NOT DISTINCT FROM $1
                AND file_id   IS NOT DISTINCT FROM $2
                AND namespace = $3
                AND local_name = $4",
        )
        .bind(folder_id)
        .bind(file_id)
        .bind(&name.namespace)
        .bind(&name.name)
        .execute(&*self.pool)
        .await
        .map_err(|e| DomainError::internal_error("DeadPropertyStore", format!("remove: {e}")))?;
        Ok(())
    }

    /// Return all dead properties for the given resource.
    pub async fn get_all(
        &self,
        r: ResourceRef,
    ) -> Result<Vec<(QualifiedName, Option<String>)>, DomainError> {
        let (folder_id, file_id) = split_ref(r);
        let rows = sqlx::query(
            "SELECT namespace, local_name, value
               FROM storage.webdav_dead_properties
              WHERE folder_id IS NOT DISTINCT FROM $1
                AND file_id   IS NOT DISTINCT FROM $2",
        )
        .bind(folder_id)
        .bind(file_id)
        .fetch_all(&*self.pool)
        .await
        .map_err(|e| DomainError::internal_error("DeadPropertyStore", format!("get_all: {e}")))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let namespace: String = r.get("namespace");
                let local_name: String = r.get("local_name");
                let value: Option<String> = r.get("value");
                (QualifiedName::new(namespace, local_name), value)
            })
            .collect())
    }

    /// Return a specific dead property, or `None` if not stored.
    /// Returns `Some(None)` when the property exists with an empty value.
    pub async fn get(
        &self,
        r: ResourceRef,
        name: &QualifiedName,
    ) -> Result<Option<Option<String>>, DomainError> {
        let (folder_id, file_id) = split_ref(r);
        let row = sqlx::query(
            "SELECT value FROM storage.webdav_dead_properties
              WHERE folder_id IS NOT DISTINCT FROM $1
                AND file_id   IS NOT DISTINCT FROM $2
                AND namespace = $3
                AND local_name = $4",
        )
        .bind(folder_id)
        .bind(file_id)
        .bind(&name.namespace)
        .bind(&name.name)
        .fetch_optional(&*self.pool)
        .await
        .map_err(|e| DomainError::internal_error("DeadPropertyStore", format!("get: {e}")))?;

        Ok(row.map(|r| r.get::<Option<String>, _>("value")))
    }
}

/// Splits a `ResourceRef` into `(folder_id, file_id)` Option pairs for
/// binding into SQL. The unused slot is `None` so `IS NOT DISTINCT FROM`
/// matches the NULL stored in the unused column.
fn split_ref(r: ResourceRef) -> (Option<Uuid>, Option<Uuid>) {
    match r {
        ResourceRef::Folder(id) => (Some(id), None),
        ResourceRef::File(id) => (None, Some(id)),
    }
}

pub fn create_dead_property_store(pool: Arc<PgPool>) -> Arc<DeadPropertyStore> {
    Arc::new(DeadPropertyStore::new(pool))
}
