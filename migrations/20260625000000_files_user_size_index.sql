-- Covering partial index for per-user storage-usage accounting.
--
-- The usage calculation is:
--   SELECT COALESCE(SUM(size), 0) FROM storage.files
--    WHERE user_id = $1 AND NOT is_trashed;
--
-- Without an index that carries `size`, this is a heap scan over every file
-- the user owns. This index lets PostgreSQL satisfy it with an index-only scan:
--   * keyed by user_id          → only the target user's rows are visited
--   * INCLUDE (size)            → the sum is read straight from the index
--   * WHERE NOT is_trashed      → matches the query predicate exactly and keeps
--                                 the index small (trashed files are excluded)
--
-- Used by the per-upload usage update and the periodic background
-- reconciliation sweep (GET /api/auth/me no longer recomputes usage inline).
CREATE INDEX IF NOT EXISTS idx_files_user_size_active
    ON storage.files (user_id) INCLUDE (size)
    WHERE NOT is_trashed;
