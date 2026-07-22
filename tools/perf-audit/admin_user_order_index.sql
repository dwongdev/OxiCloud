\set ON_ERROR_STOP on
\pset pager off
\pset format unaligned
\pset tuples_only on

-- SUPERSEDED EXPLORATORY FIXTURE: every generated row shares one timestamp,
-- so PostgreSQL posting-list compression understates representative btree
-- storage cost. Keep this file only as raw historical evidence; do not use it
-- to accept or reject an index. Use admin_user_order_index_representative.sql.

-- A/B the missing ORDER BY index used by GET /api/admin/users. Two temporary
-- tables keep both variants resident and let samples alternate without DDL
-- contaminating timings. No persistent state survives the transaction.
BEGIN;

CREATE TEMP TABLE perf_users_no_index (
    id uuid NOT NULL,
    username text,
    email text NOT NULL,
    role_text text NOT NULL,
    storage_quota_bytes bigint NOT NULL,
    storage_used_bytes bigint NOT NULL,
    created_at timestamptz NOT NULL,
    last_login_at timestamptz,
    active boolean NOT NULL,
    oidc_provider text,
    is_external boolean NOT NULL
);

INSERT INTO perf_users_no_index
SELECT
    gen_random_uuid(),
    'user-' || n,
    'user-' || n || '@example.invalid',
    CASE WHEN n % 20 = 0 THEN 'admin' ELSE 'user' END,
    10737418240,
    n::bigint * 1048576,
    -- One hundred accounts intentionally share each timestamp.  The real
    -- default is statement-stable CURRENT_TIMESTAMP, so bulk/JIT creation can
    -- produce ties; `id` must make page boundaries deterministic.
    timestamptz '2026-01-01 00:00:00+00' + make_interval(secs => n / 100),
    timestamptz '2026-01-01 00:00:00+00' + make_interval(secs => n / 2),
    true,
    CASE WHEN n % 3 = 0 THEN 'keycloak' END,
    n % 7 = 0
FROM generate_series(1, 500000) AS n;

CREATE TEMP TABLE perf_users_indexed
    (LIKE perf_users_no_index INCLUDING ALL);
INSERT INTO perf_users_indexed SELECT * FROM perf_users_no_index;
CREATE INDEX perf_users_indexed_created_at_id
    ON perf_users_indexed (created_at DESC, id DESC);

ANALYZE perf_users_no_index;
ANALYZE perf_users_indexed;

SELECT 'created_at_id_index_bytes|' || pg_relation_size('perf_users_indexed_created_at_id');
SELECT 'tied_timestamp_groups|' || COUNT(*)
FROM (
    SELECT created_at FROM perf_users_no_index GROUP BY created_at HAVING COUNT(*) > 1
) tied;
SELECT 'stable_order_match|' || (
    ARRAY(
        SELECT id FROM perf_users_no_index
        ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900
    ) = ARRAY(
        SELECT id FROM perf_users_indexed
        ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900
    )
);
SELECT 'adjacent_page_overlap|' || COUNT(*)
FROM (
    SELECT id FROM perf_users_indexed
    ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 0
) first_page
JOIN (
    SELECT id FROM perf_users_indexed
    ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 100
) second_page USING (id);

\o /dev/null
\timing on

-- Warm both table variants and both page depths.
\echo no_index_warmup_first
SELECT id, username, email, role_text, storage_quota_bytes, storage_used_bytes,
       last_login_at, active, oidc_provider, is_external
FROM perf_users_no_index ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 0;
\echo indexed_warmup_first
SELECT id, username, email, role_text, storage_quota_bytes, storage_used_bytes,
       last_login_at, active, oidc_provider, is_external
FROM perf_users_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 0;
\echo no_index_warmup_deep
SELECT id, username, email, role_text, storage_quota_bytes, storage_used_bytes,
       last_login_at, active, oidc_provider, is_external
FROM perf_users_no_index ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo indexed_warmup_deep
SELECT id, username, email, role_text, storage_quota_bytes, storage_used_bytes,
       last_login_at, active, oidc_provider, is_external
FROM perf_users_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

\echo no_index_first_1
SELECT * FROM perf_users_no_index ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 0;
\echo indexed_first_1
SELECT * FROM perf_users_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 0;
\echo indexed_deep_1
SELECT * FROM perf_users_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo no_index_deep_1
SELECT * FROM perf_users_no_index ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

\echo indexed_first_2
SELECT * FROM perf_users_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 0;
\echo no_index_first_2
SELECT * FROM perf_users_no_index ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 0;
\echo no_index_deep_2
SELECT * FROM perf_users_no_index ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo indexed_deep_2
SELECT * FROM perf_users_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

\echo no_index_first_3
SELECT * FROM perf_users_no_index ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 0;
\echo indexed_first_3
SELECT * FROM perf_users_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 0;
\echo indexed_deep_3
SELECT * FROM perf_users_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo no_index_deep_3
SELECT * FROM perf_users_no_index ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

\echo indexed_first_4
SELECT * FROM perf_users_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 0;
\echo no_index_first_4
SELECT * FROM perf_users_no_index ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 0;
\echo no_index_deep_4
SELECT * FROM perf_users_no_index ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo indexed_deep_4
SELECT * FROM perf_users_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

\echo no_index_first_5
SELECT * FROM perf_users_no_index ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 0;
\echo indexed_first_5
SELECT * FROM perf_users_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 0;
\echo indexed_deep_5
SELECT * FROM perf_users_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo no_index_deep_5
SELECT * FROM perf_users_no_index ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

-- Write-cost gate: an ordering index is not free. Copy the same 10k-row shape
-- into each variant and report the insertion tax alongside the read win.
\echo no_index_insert_1
INSERT INTO perf_users_no_index SELECT * FROM perf_users_no_index LIMIT 10000;
\echo indexed_insert_1
INSERT INTO perf_users_indexed SELECT * FROM perf_users_indexed LIMIT 10000;
\echo indexed_insert_2
INSERT INTO perf_users_indexed SELECT * FROM perf_users_indexed LIMIT 10000;
\echo no_index_insert_2
INSERT INTO perf_users_no_index SELECT * FROM perf_users_no_index LIMIT 10000;
\echo no_index_insert_3
INSERT INTO perf_users_no_index SELECT * FROM perf_users_no_index LIMIT 10000;
\echo indexed_insert_3
INSERT INTO perf_users_indexed SELECT * FROM perf_users_indexed LIMIT 10000;
\echo indexed_insert_4
INSERT INTO perf_users_indexed SELECT * FROM perf_users_indexed LIMIT 10000;
\echo no_index_insert_4
INSERT INTO perf_users_no_index SELECT * FROM perf_users_no_index LIMIT 10000;
\echo no_index_insert_5
INSERT INTO perf_users_no_index SELECT * FROM perf_users_no_index LIMIT 10000;
\echo indexed_insert_5
INSERT INTO perf_users_indexed SELECT * FROM perf_users_indexed LIMIT 10000;

\timing off
\o
ROLLBACK;
