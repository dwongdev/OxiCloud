//! PostgreSQL-backed folder repository.
//!
//! Implements `FolderRepository` (and thus `FolderStoragePort`) using the
//! `storage.folders` table.  Folders are purely virtual — no physical
//! directories are created on the filesystem.
//!
//! Folder paths are **materialized** in a `path TEXT` column maintained by
//! database triggers, so reading a folder's full path is always O(1) — no
//! recursive CTEs or N+1 queries.

use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use super::transaction_utils::retry_on_deadlock;
use crate::application::dtos::folder_dto::{FolderResourceCursor, FolderResourceRow};
use crate::common::errors::DomainError;
use crate::domain::entities::folder::Folder;
use crate::domain::repositories::folder_repository::FolderRepository;
use crate::domain::services::authorization::ResourceKind;
use crate::domain::services::path_service::StoragePath;

/// Type alias for folder metadata rows from SQL queries.
/// Tuple order: id, name, path, parent_id, user_id, created_at,
/// modified_at, tree_modified_at. The trailing `tree_modified_at`
/// feeds [`Folder::etag`] — every SELECT here must include
/// `EXTRACT(EPOCH FROM tree_modified_at)::bigint`.
type FolderRow = (String, String, String, Option<String>, Uuid, i64, i64, i64);

/// Type alias for paginated folder rows (includes total_count as
/// the last element after `tree_modified_at`).
type FolderRowPaginated = (
    String,
    String,
    String,
    Option<String>,
    Uuid,
    i64,
    i64,
    i64,
    i64,
);

/// Type alias for folder rows with optional user_id.
type FolderRowOptUser = (
    String,
    String,
    String,
    Option<String>,
    Option<Uuid>,
    i64,
    i64,
    i64,
);

/// PostgreSQL-backed folder repository.
///
/// All folder metadata lives in the `storage.folders` table.  The physical
/// filesystem is never touched for folder operations.
pub struct FolderDbRepository {
    pool: Option<Arc<PgPool>>,
}

impl FolderDbRepository {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool: Some(pool) }
    }

    /// Creates a stub instance for `AppState::default()`.
    /// This is never called in production — only used for route scaffolding.
    pub fn new_stub() -> Self {
        Self { pool: None }
    }

    /// Get the pool, panicking if stub.
    fn pool(&self) -> &PgPool {
        self.pool
            .as_deref()
            .expect("FolderDbRepository: pool not available (stub instance)")
    }

    // ── helpers ──────────────────────────────────────────────────

    /// Convert a database row into a `Folder` domain entity.
    ///
    /// The `path` comes directly from the materialized `path` column — no
    /// extra queries needed.
    #[allow(clippy::too_many_arguments)]
    fn row_to_folder(
        id: String,
        name: String,
        path: String,
        parent_id: Option<String>,
        user_id: Option<Uuid>,
        created_at: i64,
        modified_at: i64,
        tree_modified_at: i64,
    ) -> Result<Folder, DomainError> {
        let storage_path = StoragePath::from_string(&path);
        Folder::with_timestamps_and_tree(
            id,
            name,
            storage_path,
            parent_id,
            user_id,
            created_at as u64,
            modified_at as u64,
            tree_modified_at as u64,
        )
        .map_err(|e| DomainError::internal_error("FolderDb", format!("entity: {e}")))
    }
}

impl FolderRepository for FolderDbRepository {
    async fn create_folder(
        &self,
        name: String,
        parent_id: Option<String>,
    ) -> Result<Folder, DomainError> {
        // Derive user_id from parent folder.  Root-level folders require the
        // caller to have set up the home folder beforehand (done during user
        // registration).
        let user_id: Uuid = if let Some(ref pid) = parent_id {
            sqlx::query_scalar::<_, Uuid>("SELECT user_id FROM storage.folders WHERE id = $1::uuid")
                .bind(pid)
                .fetch_optional(self.pool())
                .await
                .map_err(|e| {
                    DomainError::internal_error("FolderDb", format!("parent lookup: {e}"))
                })?
                .ok_or_else(|| DomainError::not_found("Folder", pid))?
        } else {
            return Err(DomainError::internal_error(
                "FolderDb",
                "Cannot create root folder without user_id — use create_home_folder instead",
            ));
        };

        let row = sqlx::query_as::<_, (String, String, i64, i64, i64)>(
            r#"
            INSERT INTO storage.folders (name, parent_id, user_id)
            VALUES ($1, $2::uuid, $3)
            RETURNING id::text,
                      path,
                      EXTRACT(EPOCH FROM created_at)::bigint,
                      EXTRACT(EPOCH FROM updated_at)::bigint,
                      EXTRACT(EPOCH FROM tree_modified_at)::bigint
            "#,
        )
        .bind(&name)
        .bind(&parent_id)
        .bind(user_id)
        .fetch_one(self.pool())
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e
                && db_err.code().as_deref() == Some("23505")
            {
                return DomainError::already_exists(
                    "Folder",
                    format!("{name} already exists in parent"),
                );
            }
            DomainError::internal_error("FolderDb", format!("insert: {e}"))
        })?;

        Self::row_to_folder(
            row.0,
            name,
            row.1,
            parent_id,
            Some(user_id),
            row.2,
            row.3,
            row.4,
        )
    }

    async fn get_folder(&self, id: &str) -> Result<Folder, DomainError> {
        let row = sqlx::query_as::<_, FolderRow>(
            r#"
            SELECT id::text, name, path, parent_id::text, user_id,
                   EXTRACT(EPOCH FROM created_at)::bigint,
                   EXTRACT(EPOCH FROM updated_at)::bigint,
                   EXTRACT(EPOCH FROM tree_modified_at)::bigint
              FROM storage.folders
             WHERE id = $1::uuid AND NOT is_trashed
            "#,
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| DomainError::internal_error("FolderDb", format!("get: {e}")))?
        .ok_or_else(|| DomainError::not_found("Folder", id))?;

        Self::row_to_folder(row.0, row.1, row.2, row.3, Some(row.4), row.5, row.6, row.7)
    }

    async fn get_folder_by_path(&self, storage_path: &StoragePath) -> Result<Folder, DomainError> {
        let path_str = storage_path.to_string();
        // Strip leading '/' if present — DB stores "Home - user/Docs", not "/Home - user/Docs"
        let lookup = path_str.strip_prefix('/').unwrap_or(&path_str);

        if lookup.is_empty() {
            return Err(DomainError::not_found("Folder", "empty path"));
        }

        let row = sqlx::query_as::<_, FolderRow>(
            r#"
            SELECT id::text, name, path, parent_id::text, user_id,
                   EXTRACT(EPOCH FROM created_at)::bigint,
                   EXTRACT(EPOCH FROM updated_at)::bigint,
                       EXTRACT(EPOCH FROM tree_modified_at)::bigint
              FROM storage.folders
             WHERE path = $1 AND NOT is_trashed
            "#,
        )
        .bind(lookup)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| DomainError::internal_error("FolderDb", format!("path lookup: {e}")))?
        .ok_or_else(|| DomainError::not_found("Folder", lookup))?;

        Self::row_to_folder(row.0, row.1, row.2, row.3, Some(row.4), row.5, row.6, row.7)
    }

    #[allow(clippy::type_complexity)]
    async fn list_folders(&self, parent_id: Option<&str>) -> Result<Vec<Folder>, DomainError> {
        let rows: Vec<FolderRow> = if let Some(pid) = parent_id {
            sqlx::query_as(
                r#"
                SELECT id::text, name, path, parent_id::text, user_id,
                       EXTRACT(EPOCH FROM created_at)::bigint,
                       EXTRACT(EPOCH FROM updated_at)::bigint,
                       EXTRACT(EPOCH FROM tree_modified_at)::bigint
                  FROM storage.folders
                 WHERE parent_id = $1::uuid AND NOT is_trashed
                 ORDER BY name
                "#,
            )
            .bind(pid)
            .fetch_all(self.pool())
            .await
        } else {
            sqlx::query_as(
                r#"
                SELECT id::text, name, path, parent_id::text, user_id,
                       EXTRACT(EPOCH FROM created_at)::bigint,
                       EXTRACT(EPOCH FROM updated_at)::bigint,
                       EXTRACT(EPOCH FROM tree_modified_at)::bigint
                  FROM storage.folders
                 WHERE parent_id IS NULL AND NOT is_trashed
                 ORDER BY name
                "#,
            )
            .fetch_all(self.pool())
            .await
        }
        .map_err(|e| DomainError::internal_error("FolderDb", format!("list: {e}")))?;

        rows.into_iter()
            .map(|(id, name, path, pid, uid, ca, ma, tma)| {
                Self::row_to_folder(id, name, path, pid, Some(uid), ca, ma, tma)
            })
            .collect()
    }

    #[allow(clippy::type_complexity)]
    async fn list_folders_by_owner(
        &self,
        parent_id: Option<&str>,
        owner_id: Uuid,
    ) -> Result<Vec<Folder>, DomainError> {
        let rows: Vec<FolderRow> = if let Some(pid) = parent_id {
            sqlx::query_as(
                r#"
                SELECT id::text, name, path, parent_id::text, user_id,
                       EXTRACT(EPOCH FROM created_at)::bigint,
                       EXTRACT(EPOCH FROM updated_at)::bigint,
                       EXTRACT(EPOCH FROM tree_modified_at)::bigint
                  FROM storage.folders
                 WHERE parent_id = $1::uuid AND user_id = $2 AND NOT is_trashed
                 ORDER BY name
                "#,
            )
            .bind(pid)
            .bind(owner_id)
            .fetch_all(self.pool())
            .await
        } else {
            sqlx::query_as(
                r#"
                SELECT id::text, name, path, parent_id::text, user_id,
                       EXTRACT(EPOCH FROM created_at)::bigint,
                       EXTRACT(EPOCH FROM updated_at)::bigint,
                       EXTRACT(EPOCH FROM tree_modified_at)::bigint
                  FROM storage.folders
                 WHERE parent_id IS NULL AND user_id = $1 AND NOT is_trashed
                 ORDER BY name
                "#,
            )
            .bind(owner_id)
            .fetch_all(self.pool())
            .await
        }
        .map_err(|e| DomainError::internal_error("FolderDb", format!("list_by_owner: {e}")))?;

        rows.into_iter()
            .map(|(id, name, path, pid, uid, ca, ma, tma)| {
                Self::row_to_folder(id, name, path, pid, Some(uid), ca, ma, tma)
            })
            .collect()
    }

    /// Paginated folder listing — single query with `COUNT(*) OVER()` window
    /// function so the total matching count comes back alongside the data rows,
    /// eliminating a separate COUNT round-trip.
    #[allow(clippy::type_complexity)]
    async fn list_folders_paginated(
        &self,
        parent_id: Option<&str>,
        offset: usize,
        limit: usize,
        include_total: bool,
    ) -> Result<(Vec<Folder>, Option<usize>), DomainError> {
        let rows: Vec<FolderRowPaginated> = if let Some(pid) = parent_id {
            sqlx::query_as(
                r#"
                SELECT id::text, name, path, parent_id::text, user_id,
                       EXTRACT(EPOCH FROM created_at)::bigint,
                       EXTRACT(EPOCH FROM updated_at)::bigint,
                       EXTRACT(EPOCH FROM tree_modified_at)::bigint,
                       COUNT(*) OVER() AS total_count
                  FROM storage.folders
                 WHERE parent_id = $1::uuid AND NOT is_trashed
                 ORDER BY name
                 LIMIT $2 OFFSET $3
                "#,
            )
            .bind(pid)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(self.pool())
            .await
        } else {
            sqlx::query_as(
                r#"
                SELECT id::text, name, path, parent_id::text, user_id,
                       EXTRACT(EPOCH FROM created_at)::bigint,
                       EXTRACT(EPOCH FROM updated_at)::bigint,
                       EXTRACT(EPOCH FROM tree_modified_at)::bigint,
                       COUNT(*) OVER() AS total_count
                  FROM storage.folders
                 WHERE parent_id IS NULL AND NOT is_trashed
                 ORDER BY name
                 LIMIT $1 OFFSET $2
                "#,
            )
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(self.pool())
            .await
        }
        .map_err(|e| DomainError::internal_error("FolderDb", format!("paginate: {e}")))?;

        // total_count is identical in every row; 0 when the result set is empty.
        let total = if include_total {
            Some(rows.first().map_or(0, |r| r.8) as usize)
        } else {
            None
        };

        let folders: Result<Vec<Folder>, DomainError> = rows
            .into_iter()
            .map(|(id, name, path, pid, uid, ca, ma, tma, _total)| {
                Self::row_to_folder(id, name, path, pid, Some(uid), ca, ma, tma)
            })
            .collect();
        Ok((folders?, total))
    }

    /// Paginated folder listing filtered by owner — single query with
    /// `COUNT(*) OVER()` to avoid a separate COUNT round-trip.
    #[allow(clippy::type_complexity)]
    async fn list_folders_by_owner_paginated(
        &self,
        parent_id: Option<&str>,
        owner_id: Uuid,
        offset: usize,
        limit: usize,
        include_total: bool,
    ) -> Result<(Vec<Folder>, Option<usize>), DomainError> {
        let rows: Vec<FolderRowPaginated> = if let Some(pid) = parent_id {
            sqlx::query_as(
                r#"
                SELECT id::text, name, path, parent_id::text, user_id,
                       EXTRACT(EPOCH FROM created_at)::bigint,
                       EXTRACT(EPOCH FROM updated_at)::bigint,
                       EXTRACT(EPOCH FROM tree_modified_at)::bigint,
                       COUNT(*) OVER() AS total_count
                  FROM storage.folders
                 WHERE parent_id = $1::uuid AND user_id = $2 AND NOT is_trashed
                 ORDER BY name
                 LIMIT $3 OFFSET $4
                "#,
            )
            .bind(pid)
            .bind(owner_id)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(self.pool())
            .await
        } else {
            sqlx::query_as(
                r#"
                SELECT id::text, name, path, parent_id::text, user_id,
                       EXTRACT(EPOCH FROM created_at)::bigint,
                       EXTRACT(EPOCH FROM updated_at)::bigint,
                       EXTRACT(EPOCH FROM tree_modified_at)::bigint,
                       COUNT(*) OVER() AS total_count
                  FROM storage.folders
                 WHERE parent_id IS NULL AND user_id = $1 AND NOT is_trashed
                 ORDER BY name
                 LIMIT $2 OFFSET $3
                "#,
            )
            .bind(owner_id)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(self.pool())
            .await
        }
        .map_err(|e| DomainError::internal_error("FolderDb", format!("paginate_by_owner: {e}")))?;

        let total = if include_total {
            Some(rows.first().map_or(0, |r| r.8) as usize)
        } else {
            None
        };

        let folders: Result<Vec<Folder>, DomainError> = rows
            .into_iter()
            .map(|(id, name, path, pid, uid, ca, ma, tma, _total)| {
                Self::row_to_folder(id, name, path, pid, Some(uid), ca, ma, tma)
            })
            .collect();
        Ok((folders?, total))
    }

    async fn rename_folder(&self, id: &str, new_name: String) -> Result<Folder, DomainError> {
        // The BEFORE UPDATE trigger recomputes path/lpath for this row;
        // the AFTER UPDATE cascade trigger then batch-updates all
        // descendants in a single UPDATE using the GiST lpath index.
        // That multi-row rewrite can deadlock against the tree-ETag
        // flusher's id-ordered ancestor bump — retry instead of failing
        // the user's operation (40P01 only; 23505 still maps below).
        let row = retry_on_deadlock("folders.rename", || {
            sqlx::query_as::<_, FolderRow>(
                r#"
                UPDATE storage.folders
                   SET name = $1, updated_at = NOW()
                 WHERE id = $2::uuid AND NOT is_trashed
                RETURNING id::text, name, path, parent_id::text, user_id,
                          EXTRACT(EPOCH FROM created_at)::bigint,
                          EXTRACT(EPOCH FROM updated_at)::bigint,
                           EXTRACT(EPOCH FROM tree_modified_at)::bigint
                "#,
            )
            .bind(&new_name)
            .bind(id)
            .fetch_optional(self.pool())
        })
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e
                && db_err.code().as_deref() == Some("23505")
            {
                return DomainError::already_exists("Folder", format!("{new_name} already exists"));
            }
            DomainError::internal_error("FolderDb", format!("rename: {e}"))
        })?
        .ok_or_else(|| DomainError::not_found("Folder", id))?;

        Self::row_to_folder(row.0, row.1, row.2, row.3, Some(row.4), row.5, row.6, row.7)
    }

    async fn move_folder(
        &self,
        id: &str,
        new_parent_id: Option<&str>,
    ) -> Result<Folder, DomainError> {
        // The BEFORE UPDATE trigger recomputes path/lpath for this row;
        // the AFTER UPDATE cascade trigger then batch-updates all
        // descendants in a single UPDATE using the GiST lpath index.
        // Retried on deadlock vs the tree-ETag flusher (see rename_folder).
        let row = retry_on_deadlock("folders.move", || {
            sqlx::query_as::<_, FolderRow>(
                r#"
                UPDATE storage.folders
                   SET parent_id = $1::uuid, updated_at = NOW()
                 WHERE id = $2::uuid AND NOT is_trashed
                RETURNING id::text, name, path, parent_id::text, user_id,
                          EXTRACT(EPOCH FROM created_at)::bigint,
                          EXTRACT(EPOCH FROM updated_at)::bigint,
                           EXTRACT(EPOCH FROM tree_modified_at)::bigint
                "#,
            )
            .bind(new_parent_id)
            .bind(id)
            .fetch_optional(self.pool())
        })
        .await
        .map_err(|e| DomainError::internal_error("FolderDb", format!("move: {e}")))?
        .ok_or_else(|| DomainError::not_found("Folder", id))?;

        Self::row_to_folder(row.0, row.1, row.2, row.3, Some(row.4), row.5, row.6, row.7)
    }

    async fn delete_folder(&self, id: &str) -> Result<(), DomainError> {
        // Delete all files whose folder is anywhere in the subtree.
        // Uses the GiST-indexed ltree `<@` operator — O(log N) vs the
        // O(depth × N) recursive CTE it replaces.
        // Both statements retried on deadlock vs the tree-ETag flusher's
        // id-ordered ancestor bump (multi-row exclusive locks).
        retry_on_deadlock("folders.delete_files", || {
            sqlx::query(
                "DELETE FROM storage.files \
                  WHERE folder_id IN ( \
                      SELECT id FROM storage.folders \
                       WHERE lpath <@ (SELECT lpath FROM storage.folders WHERE id = $1::uuid) \
                  )",
            )
            .bind(id)
            .execute(self.pool())
        })
        .await
        .map_err(|e| DomainError::internal_error("FolderDb", format!("delete files: {e}")))?;

        // Then delete the folder (CASCADE will remove descendant folders)
        let result = retry_on_deadlock("folders.delete", || {
            sqlx::query("DELETE FROM storage.folders WHERE id = $1::uuid")
                .bind(id)
                .execute(self.pool())
        })
        .await
        .map_err(|e| DomainError::internal_error("FolderDb", format!("delete: {e}")))?;

        if result.rows_affected() == 0 {
            return Err(DomainError::not_found("Folder", id));
        }
        Ok(())
    }

    async fn folder_exists(&self, storage_path: &StoragePath) -> Result<bool, DomainError> {
        let path_str = storage_path.to_string();
        let lookup = path_str.strip_prefix('/').unwrap_or(&path_str);

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM storage.folders WHERE path = $1 AND NOT is_trashed)",
        )
        .bind(lookup)
        .fetch_one(self.pool())
        .await
        .map_err(|e| DomainError::internal_error("FolderDb", format!("exists: {e}")))?;

        Ok(exists)
    }

    async fn get_folder_path(&self, id: &str) -> Result<StoragePath, DomainError> {
        let path: String =
            sqlx::query_scalar("SELECT path FROM storage.folders WHERE id = $1::uuid")
                .bind(id)
                .fetch_optional(self.pool())
                .await
                .map_err(|e| DomainError::internal_error("FolderDb", format!("get_path: {e}")))?
                .ok_or_else(|| DomainError::not_found("Folder", id))?;

        Ok(StoragePath::from_string(&path))
    }

    // ── Trash operations ──

    async fn move_to_trash(&self, folder_id: &str) -> Result<(), DomainError> {
        // Soft-delete the whole subtree in one statement: the root flips
        // `is_trashed` and records `original_parent_id` so restore knows
        // where to put it back; every descendant (folder or file) that
        // wasn't already in trash flips `is_trashed` too but leaves the
        // `original_*` column NULL. That NULL is the marker the restore
        // path uses to tell "cascade-trashed with the root" from
        // "independently trashed earlier" — the latter must stay in
        // trash even when the root is restored.
        //
        // Without this cascade, descendants used to remain `is_trashed = false`
        // and stay directly addressable by their full path (PROPFIND on
        // `/g9-tree/file.txt` still resolved 207 even though the parent
        // collection was gone) — a class of data-integrity drift that
        // confused desktop-sync tree walks.
        let result = retry_on_deadlock("folders.trash", || {
            sqlx::query_scalar::<_, i64>(
                r#"
                WITH trash_root AS (
                    UPDATE storage.folders
                       SET is_trashed = TRUE,
                           trashed_at = NOW(),
                           original_parent_id = parent_id,
                           updated_at = NOW()
                     WHERE id = $1::uuid AND NOT is_trashed
                    RETURNING id, lpath
                ),
                trash_descendant_folders AS (
                    UPDATE storage.folders f
                       SET is_trashed = TRUE,
                           trashed_at = NOW(),
                           updated_at = NOW()
                      FROM trash_root tr
                     WHERE f.lpath <@ tr.lpath
                       AND f.id != tr.id
                       AND NOT f.is_trashed
                    RETURNING 1
                ),
                trash_descendant_files AS (
                    UPDATE storage.files fi
                       SET is_trashed = TRUE,
                           trashed_at = NOW(),
                           updated_at = NOW()
                      FROM trash_root tr
                      JOIN storage.folders f ON f.lpath <@ tr.lpath
                     WHERE fi.folder_id = f.id
                       AND NOT fi.is_trashed
                    RETURNING 1
                )
                SELECT COUNT(*) FROM trash_root
                "#,
            )
            .bind(folder_id)
            .fetch_one(self.pool())
        })
        .await
        .map_err(|e| DomainError::internal_error("FolderDb", format!("trash: {e}")))?;

        if result == 0 {
            return Err(DomainError::not_found("Folder", folder_id));
        }

        Ok(())
    }

    async fn restore_from_trash(
        &self,
        folder_id: &str,
        _original_path: &str,
    ) -> Result<(), DomainError> {
        // Inverse of the cascade in `move_to_trash`: restore the root
        // (BEFORE UPDATE trigger recomputes path/lpath via the parent_id
        // change), then un-trash every descendant whose `original_*`
        // column is NULL — those are the rows we cascade-trashed
        // ourselves. Descendants that were independently trashed
        // *before* this folder went to trash have `original_*` set, so
        // they correctly stay in trash and continue to show up as
        // top-level trash entries via `storage.trash_items`.
        let result = retry_on_deadlock("folders.restore", || {
            sqlx::query_scalar::<_, i64>(
                r#"
                WITH restore_root AS (
                    UPDATE storage.folders
                       SET is_trashed = FALSE,
                           trashed_at = NULL,
                           parent_id = COALESCE(original_parent_id, parent_id),
                           original_parent_id = NULL,
                           updated_at = NOW()
                     WHERE id = $1::uuid AND is_trashed
                    RETURNING id, lpath
                ),
                restore_descendant_folders AS (
                    UPDATE storage.folders f
                       SET is_trashed = FALSE,
                           trashed_at = NULL,
                           updated_at = NOW()
                      FROM restore_root rr
                     WHERE f.lpath <@ rr.lpath
                       AND f.id != rr.id
                       AND f.is_trashed
                       AND f.original_parent_id IS NULL
                    RETURNING 1
                ),
                restore_descendant_files AS (
                    UPDATE storage.files fi
                       SET is_trashed = FALSE,
                           trashed_at = NULL,
                           updated_at = NOW()
                      FROM restore_root rr
                      JOIN storage.folders f ON f.lpath <@ rr.lpath
                     WHERE fi.folder_id = f.id
                       AND fi.is_trashed
                       AND fi.original_folder_id IS NULL
                    RETURNING 1
                )
                SELECT COUNT(*) FROM restore_root
                "#,
            )
            .bind(folder_id)
            .fetch_one(self.pool())
        })
        .await
        .map_err(|e| DomainError::internal_error("FolderDb", format!("restore: {e}")))?;

        if result == 0 {
            return Err(DomainError::not_found("Folder", folder_id));
        }

        Ok(())
    }

    async fn delete_folder_permanently(&self, folder_id: &str) -> Result<(), DomainError> {
        // Delete all files whose folder is anywhere in the subtree
        // (GiST ltree index, same pattern as delete_folder — both
        // statements retried on deadlock vs the tree-ETag flusher).
        retry_on_deadlock("folders.perm_delete_files", || {
            sqlx::query(
                "DELETE FROM storage.files \
                  WHERE folder_id IN ( \
                      SELECT id FROM storage.folders \
                       WHERE lpath <@ (SELECT lpath FROM storage.folders WHERE id = $1::uuid) \
                  )",
            )
            .bind(folder_id)
            .execute(self.pool())
        })
        .await
        .map_err(|e| DomainError::internal_error("FolderDb", format!("perm delete files: {e}")))?;

        // Then permanently delete folder — CASCADE handles descendant folders
        let result = retry_on_deadlock("folders.perm_delete", || {
            sqlx::query("DELETE FROM storage.folders WHERE id = $1::uuid")
                .bind(folder_id)
                .execute(self.pool())
        })
        .await
        .map_err(|e| DomainError::internal_error("FolderDb", format!("perm delete: {e}")))?;

        if result.rows_affected() == 0 {
            return Err(DomainError::not_found("Folder", folder_id));
        }
        Ok(())
    }

    async fn create_home_folder(&self, user_id: Uuid, name: String) -> Result<Folder, DomainError> {
        let row = sqlx::query_as::<_, (String, String, i64, i64, i64)>(
            r#"
            INSERT INTO storage.folders (name, parent_id, user_id)
            VALUES ($1, NULL, $2)
            ON CONFLICT DO NOTHING
            RETURNING id::text,
                      path,
                      EXTRACT(EPOCH FROM created_at)::bigint,
                      EXTRACT(EPOCH FROM updated_at)::bigint,
                       EXTRACT(EPOCH FROM tree_modified_at)::bigint
            "#,
        )
        .bind(&name)
        .bind(user_id)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| DomainError::internal_error("FolderDb", format!("home folder: {e}")))?;

        match row {
            Some((id, path, ca, ma, tma)) => {
                Self::row_to_folder(id, name.clone(), path, None, Some(user_id), ca, ma, tma)
            }
            None => {
                // Already exists — fetch it
                let existing = sqlx::query_as::<_, (String, String, i64, i64, i64)>(
                    r#"
                    SELECT id::text,
                           path,
                           EXTRACT(EPOCH FROM created_at)::bigint,
                           EXTRACT(EPOCH FROM updated_at)::bigint,
                           EXTRACT(EPOCH FROM tree_modified_at)::bigint
                      FROM storage.folders
                     WHERE name = $1 AND user_id = $2 AND parent_id IS NULL
                    "#,
                )
                .bind(&name)
                .bind(user_id)
                .fetch_one(self.pool())
                .await
                .map_err(|e| DomainError::internal_error("FolderDb", format!("home fetch: {e}")))?;
                Self::row_to_folder(
                    existing.0,
                    name,
                    existing.1,
                    None,
                    Some(user_id),
                    existing.2,
                    existing.3,
                    existing.4,
                )
            }
        }
    }

    /// Lists every folder in a subtree rooted at `folder_id` (inclusive).
    ///
    /// Single GiST-indexed query: `fo.lpath <@ (root's lpath)`.
    /// Ordered by `fo.path` so callers can iterate in directory order.
    #[allow(clippy::type_complexity)]
    async fn list_subtree_folders(&self, folder_id: &str) -> Result<Vec<Folder>, DomainError> {
        let sql = "SELECT fo.id::text, fo.name, fo.path, fo.parent_id::text, \
                          fo.user_id, \
                          EXTRACT(EPOCH FROM fo.created_at)::bigint, \
                          EXTRACT(EPOCH FROM fo.updated_at)::bigint, \
                          EXTRACT(EPOCH FROM fo.tree_modified_at)::bigint \
                     FROM storage.folders fo \
                    WHERE fo.is_trashed = false \
                      AND fo.lpath <@ (SELECT lpath FROM storage.folders WHERE id = $1::uuid) \
                    ORDER BY fo.path";

        let rows: Vec<FolderRowOptUser> = sqlx::query_as(sql)
            .bind(folder_id)
            .fetch_all(self.pool())
            .await
            .map_err(|e| {
                DomainError::internal_error("FolderDb", format!("subtree folders: {e}"))
            })?;

        rows.into_iter()
            .map(|(id, name, path, pid, uid, ca, ma, tma)| {
                Self::row_to_folder(id, name, path, pid, uid, ca, ma, tma)
            })
            .collect()
    }

    /// SQL-level folder search with name filter, user isolation, and
    /// recursive / non-recursive modes.
    ///
    /// - Non-recursive: `WHERE parent_id = $1 AND user_id = $2 [AND LIKE]`
    /// - Recursive + folder_id: delegates to `list_descendant_folders`
    /// - Recursive + no folder_id: `WHERE user_id = $1 [AND LIKE]`
    #[allow(clippy::type_complexity)]
    async fn search_folders(
        &self,
        parent_id: Option<&str>,
        name_contains: Option<&str>,
        user_id: Uuid,
        recursive: bool,
    ) -> Result<Vec<Folder>, DomainError> {
        // Recursive with folder scope → existing optimised ltree scan
        if recursive && let Some(fid) = parent_id {
            return self
                .list_descendant_folders(fid, name_contains, user_id)
                .await;
        }

        // Build optional name filter — use ILIKE (case-insensitive) so the
        // GIN trigram index idx_folders_name_trgm is used instead of a seq scan.
        let (name_clause, name_pattern) = match name_contains {
            Some(name) if name.len() >= 3 => (
                if recursive {
                    " AND fo.name ILIKE $2"
                } else {
                    " AND fo.name ILIKE $3"
                },
                Some(super::like_escape(name)),
            ),
            _ => ("", None),
        };

        if recursive {
            // Recursive, no folder scope → ALL user folders
            let sql = format!(
                "SELECT fo.id::text, fo.name, fo.path, fo.parent_id::text, \
                        fo.user_id, \
                        EXTRACT(EPOCH FROM fo.created_at)::bigint, \
                        EXTRACT(EPOCH FROM fo.updated_at)::bigint, \
                          EXTRACT(EPOCH FROM fo.tree_modified_at)::bigint \
                   FROM storage.folders fo \
                  WHERE fo.user_id = $1 \
                    AND fo.is_trashed = false \
                    {name_clause} \
                  ORDER BY fo.name"
            );

            let rows: Vec<FolderRowOptUser> = if let Some(ref pattern) = name_pattern {
                sqlx::query_as(&sql)
                    .bind(user_id)
                    .bind(pattern)
                    .fetch_all(self.pool())
                    .await
            } else {
                sqlx::query_as(&sql)
                    .bind(user_id)
                    .fetch_all(self.pool())
                    .await
            }
            .map_err(|e| DomainError::internal_error("FolderDb", format!("search_folders: {e}")))?;

            return rows
                .into_iter()
                .map(|(id, name, path, pid, uid, ca, ma, tma)| {
                    Self::row_to_folder(id, name, path, pid, uid, ca, ma, tma)
                })
                .collect();
        }

        // Non-recursive: direct children of parent_id, filtered by user
        let sql = if parent_id.is_some() {
            format!(
                "SELECT fo.id::text, fo.name, fo.path, fo.parent_id::text, \
                        fo.user_id, \
                        EXTRACT(EPOCH FROM fo.created_at)::bigint, \
                        EXTRACT(EPOCH FROM fo.updated_at)::bigint, \
                          EXTRACT(EPOCH FROM fo.tree_modified_at)::bigint \
                   FROM storage.folders fo \
                  WHERE fo.parent_id = $1::uuid \
                    AND fo.user_id = $2 \
                    AND fo.is_trashed = false \
                    {name_clause} \
                  ORDER BY fo.name"
            )
        } else {
            // Root folders: parent_id IS NULL, reindex params ($1=user_id, $2=pattern)
            let name_clause_root = match name_contains {
                Some(name) if name.len() >= 3 => " AND fo.name ILIKE $2",
                _ => "",
            };
            format!(
                "SELECT fo.id::text, fo.name, fo.path, fo.parent_id::text, \
                        fo.user_id, \
                        EXTRACT(EPOCH FROM fo.created_at)::bigint, \
                        EXTRACT(EPOCH FROM fo.updated_at)::bigint, \
                          EXTRACT(EPOCH FROM fo.tree_modified_at)::bigint \
                   FROM storage.folders fo \
                  WHERE fo.parent_id IS NULL \
                    AND fo.user_id = $1 \
                    AND fo.is_trashed = false \
                    {name_clause_root} \
                  ORDER BY fo.name"
            )
        };

        let rows: Vec<FolderRowOptUser> = if let Some(pid) = parent_id {
            if let Some(ref pattern) = name_pattern {
                sqlx::query_as(&sql)
                    .bind(pid)
                    .bind(user_id)
                    .bind(pattern)
                    .fetch_all(self.pool())
                    .await
            } else {
                sqlx::query_as(&sql)
                    .bind(pid)
                    .bind(user_id)
                    .fetch_all(self.pool())
                    .await
            }
        } else if let Some(ref pattern) = name_pattern {
            sqlx::query_as(&sql)
                .bind(user_id)
                .bind(pattern)
                .fetch_all(self.pool())
                .await
        } else {
            sqlx::query_as(&sql)
                .bind(user_id)
                .fetch_all(self.pool())
                .await
        }
        .map_err(|e| DomainError::internal_error("FolderDb", format!("search_folders: {e}")))?;

        rows.into_iter()
            .map(|(id, name, path, pid, uid, ca, ma, tma)| {
                Self::row_to_folder(id, name, path, pid, uid, ca, ma, tma)
            })
            .collect()
    }

    /// Lists all descendant folders in a subtree using ltree GiST index.
    ///
    /// Single SQL query: `fo.lpath <@ (root's lpath)` fetches the entire
    /// subtree in one indexed scan. Optional name filter is pushed to SQL.
    #[allow(clippy::type_complexity)]
    async fn list_descendant_folders(
        &self,
        folder_id: &str,
        name_contains: Option<&str>,
        user_id: Uuid,
    ) -> Result<Vec<Folder>, DomainError> {
        let (where_extra, name_pattern) = match name_contains {
            Some(name) if name.len() >= 3 => {
                (" AND fo.name ILIKE $3", Some(super::like_escape(name)))
            }
            _ => ("", None),
        };

        let sql = format!(
            "SELECT fo.id::text, fo.name, fo.path, fo.parent_id::text, \
                    fo.user_id, \
                    EXTRACT(EPOCH FROM fo.created_at)::bigint, \
                    EXTRACT(EPOCH FROM fo.updated_at)::bigint, \
                          EXTRACT(EPOCH FROM fo.tree_modified_at)::bigint \
               FROM storage.folders fo \
              WHERE fo.user_id = $1 \
                AND fo.is_trashed = false \
                AND fo.lpath <@ (SELECT lpath FROM storage.folders WHERE id = $2::uuid) \
                AND fo.id != $2::uuid \
                {where_extra} \
              ORDER BY fo.name"
        );

        let rows: Vec<FolderRowOptUser> = if let Some(ref pattern) = name_pattern {
            sqlx::query_as(&sql)
                .bind(user_id)
                .bind(folder_id)
                .bind(pattern)
                .fetch_all(self.pool())
                .await
        } else {
            sqlx::query_as(&sql)
                .bind(user_id)
                .bind(folder_id)
                .fetch_all(self.pool())
                .await
        }
        .map_err(|e| DomainError::internal_error("FolderDb", format!("descendant search: {e}")))?;

        rows.into_iter()
            .map(|(id, name, path, pid, uid, ca, ma, tma)| {
                Self::row_to_folder(id, name, path, pid, uid, ca, ma, tma)
            })
            .collect()
    }

    #[allow(clippy::type_complexity)]
    async fn suggest_folders_by_name(
        &self,
        parent_id: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Folder>, DomainError> {
        let pattern = super::like_escape(query);
        let limit_i64 = limit as i64;

        let rows: Vec<FolderRow> = if let Some(pid) = parent_id {
            sqlx::query_as(
                r#"
                SELECT id::text, name, path, parent_id::text, user_id,
                       EXTRACT(EPOCH FROM created_at)::bigint,
                       EXTRACT(EPOCH FROM updated_at)::bigint,
                       EXTRACT(EPOCH FROM tree_modified_at)::bigint
                  FROM storage.folders
                 WHERE parent_id = $1::uuid
                   AND NOT is_trashed
                   AND name ILIKE $2
                 ORDER BY CASE
                            WHEN name ILIKE $3 THEN 0
                            WHEN name ILIKE $3 || '%' THEN 1
                            ELSE 2
                          END,
                          name
                 LIMIT $4
                "#,
            )
            .bind(pid)
            .bind(&pattern)
            .bind(query)
            .bind(limit_i64)
            .fetch_all(self.pool())
            .await
        } else {
            sqlx::query_as(
                r#"
                SELECT id::text, name, path, parent_id::text, user_id,
                       EXTRACT(EPOCH FROM created_at)::bigint,
                       EXTRACT(EPOCH FROM updated_at)::bigint,
                       EXTRACT(EPOCH FROM tree_modified_at)::bigint
                  FROM storage.folders
                 WHERE parent_id IS NULL
                   AND NOT is_trashed
                   AND name ILIKE $1
                 ORDER BY CASE
                            WHEN name ILIKE $2 THEN 0
                            WHEN name ILIKE $2 || '%' THEN 1
                            ELSE 2
                          END,
                          name
                 LIMIT $3
                "#,
            )
            .bind(&pattern)
            .bind(query)
            .bind(limit_i64)
            .fetch_all(self.pool())
            .await
        }
        .map_err(|e| DomainError::internal_error("FolderDb", format!("suggest: {e}")))?;

        rows.into_iter()
            .map(|(id, name, path, pid, uid, ca, ma, tma)| {
                Self::row_to_folder(id, name, path, pid, Some(uid), ca, ma, tma)
            })
            .collect()
    }

    async fn is_folder_in_subtree(
        &self,
        candidate_folder_id: &str,
        root_folder_id: &str,
    ) -> Result<bool, DomainError> {
        let (Ok(candidate_uuid), Ok(root_uuid)) = (
            Uuid::parse_str(candidate_folder_id),
            Uuid::parse_str(root_folder_id),
        ) else {
            return Ok(false);
        };

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (\
                 SELECT 1 \
                 FROM storage.folders c, storage.folders r \
                 WHERE c.id = $1 \
                   AND r.id = $2 \
                   AND c.is_trashed = false \
                   AND r.is_trashed = false \
                   AND c.lpath <@ r.lpath \
             )",
        )
        .bind(candidate_uuid)
        .bind(root_uuid)
        .fetch_one(self.pool())
        .await
        .map_err(|e| {
            DomainError::internal_error("FolderDb", format!("is_folder_in_subtree: {e}"))
        })?;
        Ok(exists)
    }

    async fn is_file_in_subtree(
        &self,
        file_id: &str,
        root_folder_id: &str,
    ) -> Result<bool, DomainError> {
        let (Ok(file_uuid), Ok(root_uuid)) =
            (Uuid::parse_str(file_id), Uuid::parse_str(root_folder_id))
        else {
            return Ok(false);
        };

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (\
                 SELECT 1 \
                 FROM storage.files f \
                 JOIN storage.folders parent ON f.folder_id = parent.id \
                 JOIN storage.folders root   ON root.id = $2 \
                 WHERE f.id = $1 \
                   AND f.is_trashed = false \
                   AND parent.is_trashed = false \
                   AND root.is_trashed = false \
                   AND parent.lpath <@ root.lpath \
             )",
        )
        .bind(file_uuid)
        .bind(root_uuid)
        .fetch_one(self.pool())
        .await
        .map_err(|e| DomainError::internal_error("FolderDb", format!("is_file_in_subtree: {e}")))?;
        Ok(exists)
    }
}

// ── Extra helpers for blob-storage bootstrap ──

impl FolderDbRepository {
    /// Returns user_id for a given folder. Used by file repositories.
    pub async fn get_folder_user_id(&self, folder_id: &str) -> Result<Uuid, DomainError> {
        sqlx::query_scalar::<_, Uuid>("SELECT user_id FROM storage.folders WHERE id = $1::uuid")
            .bind(folder_id)
            .fetch_optional(self.pool())
            .await
            .map_err(|e| DomainError::internal_error("FolderDb", format!("user_id lookup: {e}")))?
            .ok_or_else(|| DomainError::not_found("Folder", folder_id))
    }

    /// Verifies that `folder_id` is owned by `owner_id`.
    ///
    /// Returns `DomainError::not_found(...)` for both "folder missing" and
    /// "folder owned by someone else" — same error to avoid leaking the
    /// existence of resources belonging to other users.
    pub async fn verify_owner(&self, folder_id: &str, owner_id: Uuid) -> Result<(), DomainError> {
        let actual = self.get_folder_user_id(folder_id).await?;
        if actual != owner_id {
            return Err(DomainError::not_found(
                "Folder",
                "Target folder not found or access denied",
            ));
        }
        Ok(())
    }

    /// Cursor-paginated combined listing of sub-folders and files inside
    /// `parent_id`, sorted by `order_by`.
    ///
    /// **Authorization must be verified by the caller** before invoking this
    /// method — no ownership filter is applied here.
    ///
    /// Fetches `limit` rows (caller should pass `desired_page_size + 1` to
    /// detect the existence of a next page).  Returns raw [`FolderResourceRow`]
    /// values; the handler / service layer converts them to DTOs.
    pub async fn list_resources_paged(
        &self,
        parent_id: Uuid,
        limit: usize,
        cursor: Option<&FolderResourceCursor>,
        order_by: &str,
        kinds: Option<&[ResourceKind]>,
        reverse: bool,
    ) -> Result<Vec<FolderResourceRow>, DomainError> {
        let include_folders = kinds.is_none_or(|k| k.contains(&ResourceKind::Folder));
        let include_files = kinds.is_none_or(|k| k.contains(&ResourceKind::File));

        if !include_folders && !include_files {
            return Ok(Vec::new());
        }

        // ── CTE branches ────────────────────────────────────────────────────
        let folder_branch = r#"
            SELECT
                'folder'::text            AS resource_type,
                f.id,
                f.name,
                f.parent_id               AS folder_id,
                NULL::text                AS mime_type,
                -1::bigint                AS size,
                f.created_at,
                f.updated_at              AS modified_at,
                f.user_id,
                NULL::text                AS blob_hash,
                LOWER(f.name)             AS sort_str,
                0::bigint                 AS type_order,
                0::int                    AS folder_first
            FROM storage.folders f
            WHERE f.parent_id = $1::uuid AND NOT f.is_trashed
        "#;

        let file_branch = r#"
            SELECT
                'file'::text              AS resource_type,
                fm.id,
                fm.name,
                fm.folder_id,
                fm.mime_type,
                fm.size::bigint,
                fm.created_at,
                fm.updated_at             AS modified_at,
                fm.user_id,
                fm.blob_hash,
                LOWER(fm.name)            AS sort_str,
                fm.category_order::bigint AS type_order,
                1::int                    AS folder_first
            FROM storage.files fm
            WHERE fm.folder_id = $1::uuid AND NOT fm.is_trashed
        "#;

        let cte_inner = match (include_folders, include_files) {
            (true, true) => format!("{folder_branch} UNION ALL {file_branch}"),
            (true, false) => folder_branch.to_owned(),
            (false, true) => file_branch.to_owned(),
            (false, false) => unreachable!(),
        };

        // ── Cursor binds ─────────────────────────────────────────────────────
        // $1 = parent_id   $2 = cursor_str   $3 = cursor_int
        // $4 = cursor_ts   $5 = cursor_id    $6 = limit
        let cursor_str = cursor.and_then(|c| c.sort_str.clone());
        let cursor_int = cursor.and_then(|c| c.sort_int);
        let cursor_ts = cursor.and_then(|c| c.sort_ts);
        let cursor_id = cursor.map(|c| c.resource_id);

        // ── Sort-specific WHERE + ORDER BY ───────────────────────────────────
        // Each arm produces two variants based on `reverse`.
        // For "name": folder_first stays ASC in both directions (folders always
        // precede files); only the alpha order within each group flips.
        let (where_clause, order_clause) = match order_by {
            "type" => {
                if reverse {
                    (
                        r#"WHERE ($3::bigint IS NULL)
                              OR (type_order < $3)
                              OR (type_order = $3 AND sort_str < $2)
                              OR (type_order = $3 AND sort_str = $2 AND id < $5::uuid)"#,
                        "ORDER BY type_order DESC, sort_str DESC, id DESC",
                    )
                } else {
                    (
                        r#"WHERE ($3::bigint IS NULL)
                              OR (type_order > $3)
                              OR (type_order = $3 AND sort_str > $2)
                              OR (type_order = $3 AND sort_str = $2 AND id > $5::uuid)"#,
                        "ORDER BY type_order ASC, sort_str ASC, id ASC",
                    )
                }
            }
            "modified_at" => {
                if reverse {
                    (
                        r#"WHERE ($4::timestamptz IS NULL)
                              OR (modified_at > $4)
                              OR (modified_at = $4 AND id > $5::uuid)"#,
                        "ORDER BY modified_at ASC, id ASC",
                    )
                } else {
                    (
                        r#"WHERE ($4::timestamptz IS NULL)
                              OR (modified_at < $4)
                              OR (modified_at = $4 AND id < $5::uuid)"#,
                        "ORDER BY modified_at DESC, id DESC",
                    )
                }
            }
            "created_at" => {
                if reverse {
                    (
                        r#"WHERE ($4::timestamptz IS NULL)
                              OR (created_at > $4)
                              OR (created_at = $4 AND id > $5::uuid)"#,
                        "ORDER BY created_at ASC, id ASC",
                    )
                } else {
                    (
                        r#"WHERE ($4::timestamptz IS NULL)
                              OR (created_at < $4)
                              OR (created_at = $4 AND id < $5::uuid)"#,
                        "ORDER BY created_at DESC, id DESC",
                    )
                }
            }
            "size" => {
                if reverse {
                    (
                        r#"WHERE ($3::bigint IS NULL)
                              OR (size < $3)
                              OR (size = $3 AND id < $5::uuid)"#,
                        "ORDER BY size DESC, id DESC",
                    )
                } else {
                    (
                        r#"WHERE ($3::bigint IS NULL)
                              OR (size > $3)
                              OR (size = $3 AND id > $5::uuid)"#,
                        "ORDER BY size ASC, id ASC",
                    )
                }
            }
            _ => {
                // "name" (default): folder_first stays ASC so folders always precede
                // files; only the alpha order within each group flips when reversed.
                if reverse {
                    (
                        r#"WHERE ($3::bigint IS NULL)
                              OR (folder_first::bigint > $3)
                              OR (folder_first::bigint = $3 AND sort_str < $2)
                              OR (folder_first::bigint = $3 AND sort_str = $2 AND id < $5::uuid)"#,
                        "ORDER BY folder_first ASC, sort_str DESC, id DESC",
                    )
                } else {
                    (
                        r#"WHERE ($3::bigint IS NULL)
                              OR (folder_first::bigint > $3)
                              OR (folder_first::bigint = $3 AND sort_str > $2)
                              OR (folder_first::bigint = $3 AND sort_str = $2 AND id > $5::uuid)"#,
                        "ORDER BY folder_first ASC, sort_str ASC, id ASC",
                    )
                }
            }
        };

        let sql = format!(
            "WITH resources AS ({cte_inner}) \
             SELECT resource_type, id, name, folder_id, mime_type, size, \
                    created_at, modified_at, user_id, blob_hash, sort_str, type_order, folder_first \
             FROM resources \
             {where_clause} \
             {order_clause} \
             LIMIT $6"
        );

        // Row: (resource_type, id, name, folder_id, mime_type, size,
        //        created_at, modified_at, user_id, blob_hash,
        //        sort_str, type_order, folder_first)
        type Row = (
            String,
            Uuid,
            String,
            Option<Uuid>,
            Option<String>,
            i64,
            chrono::DateTime<chrono::Utc>,
            chrono::DateTime<chrono::Utc>,
            Uuid,
            Option<String>,
            String,
            i64,
            i32,
        );

        let rows = sqlx::query_as::<_, Row>(&sql)
            .bind(parent_id)
            .bind(cursor_str)
            .bind(cursor_int)
            .bind(cursor_ts)
            .bind(cursor_id)
            .bind(limit as i64)
            .fetch_all(self.pool())
            .await
            .map_err(|e| {
                DomainError::internal_error("FolderDb", format!("list_resources_paged: {e}"))
            })?;

        Ok(rows
            .into_iter()
            .map(|r| FolderResourceRow {
                resource_type: r.0,
                id: r.1,
                name: r.2,
                parent_id: r.3,
                mime_type: r.4,
                size: r.5,
                created_at: r.6,
                modified_at: r.7,
                owner_id: r.8,
                blob_hash: r.9,
                sort_str: r.10,
                type_order: r.11,
                folder_first: r.12,
            })
            .collect())
    }
}
