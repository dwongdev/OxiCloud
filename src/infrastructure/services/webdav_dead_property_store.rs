//! PostgreSQL-backed dead property store for WebDAV PROPPATCH / PROPFIND compliance.
//!
//! RFC 4918 §4.2 defines "dead properties" as those stored verbatim by the
//! server without interpreting their value. Properties are persisted to
//! `storage.webdav_dead_properties` and survive server restarts.

use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

use crate::application::adapters::webdav_adapter::QualifiedName;
use crate::domain::errors::DomainError;

pub struct DeadPropertyStore {
    pool: Arc<PgPool>,
}

impl DeadPropertyStore {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// Upsert a dead property. `value = None` means an empty XML element.
    pub async fn set(
        &self,
        path: &str,
        user_id: Uuid,
        name: QualifiedName,
        value: Option<String>,
    ) -> Result<(), DomainError> {
        sqlx::query!(
            r#"
            INSERT INTO storage.webdav_dead_properties
                (resource_path, user_id, namespace, local_name, value)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (resource_path, user_id, namespace, local_name)
            DO UPDATE SET value = EXCLUDED.value, updated_at = CURRENT_TIMESTAMP
            "#,
            path,
            user_id,
            name.namespace,
            name.name,
            value,
        )
        .execute(&*self.pool)
        .await
        .map_err(|e| DomainError::internal_error("DeadPropertyStore", format!("set: {e}")))?;
        Ok(())
    }

    /// Delete a specific dead property. No-op if not present.
    pub async fn remove(
        &self,
        path: &str,
        user_id: Uuid,
        name: &QualifiedName,
    ) -> Result<(), DomainError> {
        sqlx::query!(
            "DELETE FROM storage.webdav_dead_properties
             WHERE resource_path = $1 AND user_id = $2
               AND namespace = $3 AND local_name = $4",
            path,
            user_id,
            name.namespace,
            name.name,
        )
        .execute(&*self.pool)
        .await
        .map_err(|e| DomainError::internal_error("DeadPropertyStore", format!("remove: {e}")))?;
        Ok(())
    }

    /// Return all dead properties for `path`.
    pub async fn get_all(
        &self,
        path: &str,
        user_id: Uuid,
    ) -> Result<Vec<(QualifiedName, Option<String>)>, DomainError> {
        let rows = sqlx::query!(
            "SELECT namespace, local_name, value
             FROM storage.webdav_dead_properties
             WHERE resource_path = $1 AND user_id = $2",
            path,
            user_id,
        )
        .fetch_all(&*self.pool)
        .await
        .map_err(|e| DomainError::internal_error("DeadPropertyStore", format!("get_all: {e}")))?;

        Ok(rows
            .into_iter()
            .map(|r| (QualifiedName::new(r.namespace, r.local_name), r.value))
            .collect())
    }

    /// Return a specific dead property, or `None` if not stored.
    /// Returns `Some(None)` when the property exists with an empty value.
    pub async fn get(
        &self,
        path: &str,
        user_id: Uuid,
        name: &QualifiedName,
    ) -> Result<Option<Option<String>>, DomainError> {
        let row = sqlx::query!(
            "SELECT value FROM storage.webdav_dead_properties
             WHERE resource_path = $1 AND user_id = $2
               AND namespace = $3 AND local_name = $4",
            path,
            user_id,
            name.namespace,
            name.name,
        )
        .fetch_optional(&*self.pool)
        .await
        .map_err(|e| DomainError::internal_error("DeadPropertyStore", format!("get: {e}")))?;

        Ok(row.map(|r| r.value))
    }

    /// Delete all dead properties for `path` (called on DELETE).
    pub async fn remove_resource(&self, path: &str, user_id: Uuid) -> Result<(), DomainError> {
        sqlx::query!(
            "DELETE FROM storage.webdav_dead_properties
             WHERE resource_path = $1 AND user_id = $2",
            path,
            user_id,
        )
        .execute(&*self.pool)
        .await
        .map_err(|e| {
            DomainError::internal_error("DeadPropertyStore", format!("remove_resource: {e}"))
        })?;
        Ok(())
    }

    /// Move dead properties from `old_path` to `new_path` (called on MOVE).
    /// Clears any stale properties at `new_path` first.
    pub async fn rename_resource(
        &self,
        old_path: &str,
        user_id: Uuid,
        new_path: &str,
    ) -> Result<(), DomainError> {
        let mut tx = self.pool.begin().await.map_err(|e| {
            DomainError::internal_error("DeadPropertyStore", format!("rename_resource tx: {e}"))
        })?;

        sqlx::query!(
            "DELETE FROM storage.webdav_dead_properties
             WHERE resource_path = $1 AND user_id = $2",
            new_path,
            user_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            DomainError::internal_error("DeadPropertyStore", format!("rename_resource delete: {e}"))
        })?;

        sqlx::query!(
            "UPDATE storage.webdav_dead_properties
             SET resource_path = $2
             WHERE resource_path = $1 AND user_id = $3",
            old_path,
            new_path,
            user_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            DomainError::internal_error("DeadPropertyStore", format!("rename_resource update: {e}"))
        })?;

        tx.commit().await.map_err(|e| {
            DomainError::internal_error("DeadPropertyStore", format!("rename_resource commit: {e}"))
        })?;
        Ok(())
    }
}

pub fn create_dead_property_store(pool: Arc<PgPool>) -> Arc<DeadPropertyStore> {
    Arc::new(DeadPropertyStore::new(pool))
}
