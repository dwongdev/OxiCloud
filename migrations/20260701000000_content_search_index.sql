-- Content-search indexing pipeline: durable dirty-queue + per-blob text cache.
--
-- OxiCloud's search gains an embedded Tantivy (BM25) index over file NAMES and
-- file CONTENT (extracted text from PDFs, Office documents, plain text/code).
-- The Tantivy index lives on local disk ({storage}/.search-index) and is a
-- DERIVED artifact: PostgreSQL remains the single source of truth, and the
-- index can always be rebuilt from it (the app reseeds this queue whenever the
-- on-disk index is missing or its schema version changed).
--
-- This migration installs the PG side of the pipeline:
--
--   * `storage.search_index_dirty` — append-only dirty queue, mirroring the
--     proven `storage.tree_etag_dirty` design: statement-level triggers on
--     `storage.files` only INSERT queue rows (plain heap append, zero shared
--     row locks, no unique constraints), and a single background worker in the
--     app (`ContentIndexWorker`, on the maintenance pool) drains them and
--     applies the Tantivy mutations. Trigger-based capture means EVERY write
--     surface (REST, WebDAV, NextCloud, WOPI, batch ops, trash) is covered,
--     including paths that never call the in-process lifecycle hooks, and the
--     queue survives crashes/restarts (an in-memory channel would not).
--
--   * `storage.blob_extracted_text` — per-BLOB extraction cache. Text is
--     derived from CONTENT, and content is content-addressed (BLAKE3), so
--     extraction is keyed by `blob_hash`, not by file: N copies of the same
--     PDF cost ONE extraction, and rename/move/copy never re-extract.
--     No FK on blob_hash: a file's hash may resolve to either
--     `storage.blobs` (legacy whole blob) or `storage.chunk_manifests`
--     (CDC file hash); the worker garbage-collects orphans instead.
--
-- Queue semantics:
--   * Duplicates are expected and harmless — upserts are idempotent
--     (Tantivy delete_term + add_document) and the worker dedups per drain
--     batch keeping the latest op per file.
--   * The worker deletes queue rows ONLY AFTER the Tantivy commit succeeds,
--     so a crash between drain and commit re-processes the batch (at-least-
--     once delivery; idempotency makes that safe).
--   * When content search is disabled the app still drains the queue
--     (discard-only janitor) so it cannot grow unboundedly; re-enabling
--     triggers a full reseed via the index-version check anyway.
--
-- UPDATE value filter: only changes observable by the index enqueue —
-- a rename (name), a content swap (blob_hash) or a trash transition
-- (is_trashed). The EXIF `media_sort_date` sync and other metadata-only
-- updates never enqueue. Trashed files are removed from the index (they
-- must not appear in search) and re-added on restore.

-- ── Dirty queue ──────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS storage.search_index_dirty (
    id      BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    file_id UUID NOT NULL,
    op      TEXT NOT NULL CHECK (op IN ('upsert', 'delete'))
);

-- ── Per-blob extracted text cache ────────────────────────────────────
-- `text` is NULL unless status = 'ok'. `status` is terminal per blob —
-- a failed/unsupported blob is never retried until an extractor-version
-- bump wipes the row (cheap: DELETE WHERE extractor <> current).
CREATE TABLE IF NOT EXISTS storage.blob_extracted_text (
    blob_hash    VARCHAR(64) PRIMARY KEY,
    text         TEXT,
    status       TEXT NOT NULL CHECK (status IN ('ok', 'empty', 'failed', 'too_large')),
    extractor    TEXT NOT NULL,
    extracted_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- ── File side: INSERT / DELETE ───────────────────────────────────────
-- Both triggers alias their transition table to `changed_rows`; one body
-- serves both events. TG_OP picks the queue op. INSERTs of already-trashed
-- rows (trash restores go through UPDATE) are skipped.
CREATE OR REPLACE FUNCTION storage.enqueue_search_from_files_stmt()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO storage.search_index_dirty (file_id, op)
        SELECT id, 'upsert' FROM changed_rows WHERE NOT is_trashed;
    ELSE
        INSERT INTO storage.search_index_dirty (file_id, op)
        SELECT id, 'delete' FROM changed_rows;
    END IF;

    RETURN NULL;
END;
$$;

-- ── File side: UPDATE ────────────────────────────────────────────────
-- Value filter: only rename / content swap / trash transitions enqueue.
-- (PostgreSQL forbids `AFTER UPDATE OF <cols>` with transition tables,
-- so the filter lives here.) A row trashed by the update enqueues a
-- 'delete'; everything else enqueues an 'upsert' re-index.
CREATE OR REPLACE FUNCTION storage.enqueue_search_from_files_stmt_upd()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO storage.search_index_dirty (file_id, op)
    SELECT n.id,
           CASE WHEN n.is_trashed THEN 'delete' ELSE 'upsert' END
      FROM old_rows o
      JOIN new_rows n USING (id)
     WHERE (o.name, o.blob_hash, o.is_trashed)
           IS DISTINCT FROM
           (n.name, n.blob_hash, n.is_trashed);

    RETURN NULL;
END;
$$;

-- PG 13 compatibility: DROP-then-CREATE (no CREATE OR REPLACE TRIGGER).
DROP TRIGGER IF EXISTS files_enqueue_search_ins ON storage.files;
CREATE TRIGGER files_enqueue_search_ins
    AFTER INSERT ON storage.files
    REFERENCING NEW TABLE AS changed_rows
    FOR EACH STATEMENT EXECUTE FUNCTION storage.enqueue_search_from_files_stmt();

DROP TRIGGER IF EXISTS files_enqueue_search_del ON storage.files;
CREATE TRIGGER files_enqueue_search_del
    AFTER DELETE ON storage.files
    REFERENCING OLD TABLE AS changed_rows
    FOR EACH STATEMENT EXECUTE FUNCTION storage.enqueue_search_from_files_stmt();

DROP TRIGGER IF EXISTS files_enqueue_search_upd ON storage.files;
CREATE TRIGGER files_enqueue_search_upd
    AFTER UPDATE ON storage.files
    REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION storage.enqueue_search_from_files_stmt_upd();

-- ── Backfill ─────────────────────────────────────────────────────────
-- Existing deployments: queue every live file once so the first worker
-- run indexes the historical corpus. Fresh databases enqueue nothing.
INSERT INTO storage.search_index_dirty (file_id, op)
SELECT id, 'upsert' FROM storage.files WHERE NOT is_trashed;
