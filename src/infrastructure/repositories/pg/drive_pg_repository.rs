//! PostgreSQL implementation of [`DriveRepository`].
//!
//! The repo deals only with the `storage.drives` table itself. Drive
//! membership lives in `storage.role_grants` (`resource_type='drive'`)
//! and is queried through the engine's existing grant paths;
//! `list_for_subjects` below resolves `role_grants` → `storage.drives`
//! via a single join.
//!
//! See `migrations/20260802000000_drives_schema_additive.sql` for the
//! schema and `docs/plan/drive.md` §3 / §15 for the locked design.

use std::sync::Arc;

use sqlx::{PgPool, Row, types::Uuid};

use crate::domain::entities::drive::{Drive, DriveKind};
use crate::domain::repositories::drive_repository::{
    CreatePersonalDriveInput, DriveRepository, DriveRepositoryError,
};

pub struct DrivePgRepository {
    pool: Arc<PgPool>,
}

impl DrivePgRepository {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    fn map_sqlx_err(context: &'static str, e: sqlx::Error) -> DriveRepositoryError {
        if let sqlx::Error::Database(ref dberr) = e
            && let Some(code) = dberr.code()
            && code.as_ref() == "23505"
        {
            // unique_violation. With drives, the only relevant unique is
            // the partial index `idx_drives_default_for_user_unique` —
            // surface the typed variant so the lifecycle hook can detect
            // idempotent re-runs (D0-9 calls create_personal during
            // user provisioning).
            return DriveRepositoryError::DefaultDriveAlreadyExists(dberr.to_string());
        }
        DriveRepositoryError::StorageError(format!("{context}: {e}"))
    }

    fn row_to_drive(row: &sqlx::postgres::PgRow) -> Result<Drive, DriveRepositoryError> {
        let kind_str: String = row.get("kind");
        let kind = DriveKind::from_sql(&kind_str)?;
        Ok(Drive {
            id: row.get("id"),
            name: row.get("name"),
            kind,
            default_for_user: row.get("default_for_user"),
            quota_bytes: row.get("quota_bytes"),
            used_bytes: row.get("used_bytes"),
            policies: row.get("policies"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }
}

#[async_trait::async_trait]
impl DriveRepository for DrivePgRepository {
    async fn create_personal(
        &self,
        input: CreatePersonalDriveInput,
    ) -> Result<Drive, DriveRepositoryError> {
        let default_for_user = if input.is_default {
            Some(input.owner_id)
        } else {
            None
        };
        let row = sqlx::query(
            r#"
            INSERT INTO storage.drives
                (name, kind, default_for_user, quota_bytes, policies)
            VALUES ($1, 'personal', $2, $3, '{}'::jsonb)
            RETURNING id, name, kind, default_for_user, quota_bytes,
                      used_bytes, policies, created_at, updated_at
            "#,
        )
        .bind(&input.name)
        .bind(default_for_user)
        .bind(input.quota_bytes)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| Self::map_sqlx_err("create_personal", e))?;

        Self::row_to_drive(&row)
    }

    async fn get_by_id(&self, id: Uuid) -> Result<Drive, DriveRepositoryError> {
        let row = sqlx::query(
            r#"
            SELECT id, name, kind, default_for_user, quota_bytes,
                   used_bytes, policies, created_at, updated_at
              FROM storage.drives
             WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| Self::map_sqlx_err("get_by_id", e))?
        .ok_or_else(|| DriveRepositoryError::NotFound(id.to_string()))?;

        Self::row_to_drive(&row)
    }

    async fn find_default_for_user(&self, user_id: Uuid) -> Result<Drive, DriveRepositoryError> {
        let row = sqlx::query(
            r#"
            SELECT id, name, kind, default_for_user, quota_bytes,
                   used_bytes, policies, created_at, updated_at
              FROM storage.drives
             WHERE default_for_user = $1
            "#,
        )
        .bind(user_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| Self::map_sqlx_err("find_default_for_user", e))?
        .ok_or_else(|| DriveRepositoryError::NotFound(user_id.to_string()))?;

        Self::row_to_drive(&row)
    }

    async fn list_for_subjects(
        &self,
        subject_types: &[&str],
        subject_ids: &[Uuid],
    ) -> Result<Vec<Drive>, DriveRepositoryError> {
        // Joining `role_grants` → `storage.drives` returns every drive
        // the expanded subject set can read. ORDER BY puts default
        // drives first (so the picker UI doesn't need a follow-up
        // sort), then alphabetical by name. DISTINCT collapses the
        // case where a caller has multiple role_grants on the same
        // drive (e.g. direct + group-mediated); a GROUP BY on the
        // drive id sidesteps PostgreSQL's "ORDER BY expression must
        // appear in select list" rule that `SELECT DISTINCT` imposes.
        let rows = sqlx::query(
            r#"
            SELECT d.id, d.name, d.kind, d.default_for_user,
                   d.quota_bytes, d.used_bytes, d.policies,
                   d.created_at, d.updated_at
              FROM storage.drives d
              JOIN storage.role_grants g
                ON g.resource_type = 'drive'
               AND g.resource_id   = d.id
             WHERE g.subject_type = ANY($1)
               AND g.subject_id   = ANY($2)
               AND (g.expires_at IS NULL OR g.expires_at > NOW())
             GROUP BY d.id, d.name, d.kind, d.default_for_user,
                      d.quota_bytes, d.used_bytes, d.policies,
                      d.created_at, d.updated_at
             ORDER BY (d.default_for_user IS NULL) ASC,
                      LOWER(d.name) ASC
            "#,
        )
        .bind(
            subject_types
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )
        .bind(subject_ids)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| Self::map_sqlx_err("list_for_subjects", e))?;

        rows.iter().map(Self::row_to_drive).collect()
    }
}
