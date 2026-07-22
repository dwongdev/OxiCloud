\set ON_ERROR_STOP on
\pset pager off
\pset format unaligned
\pset tuples_only on

-- SUPERSEDED EXPLORATORY FIXTURE: the tied-timestamp distribution is useful as
-- a stress shape, but PostgreSQL posting-list compression makes its index-size
-- result non-representative. Use admin_user_order_index_representative.sql for
-- decisions; this file is retained only as historical evidence.

-- Three-way Pareto gate for the stable admin pagination order:
--   A. no index;
--   B. created_at only (smaller btree + incremental sort inside ties);
--   C. created_at,id (fully ordered scan).
-- One hundred rows share each timestamp to reproduce CURRENT_TIMESTAMP ties.
BEGIN;

CREATE TEMP TABLE perf_users_base (
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

INSERT INTO perf_users_base
SELECT
    gen_random_uuid(),
    'user-' || n,
    'user-' || n || '@example.invalid',
    CASE WHEN n % 20 = 0 THEN 'admin' ELSE 'user' END,
    10737418240,
    n::bigint * 1048576,
    timestamptz '2026-01-01 00:00:00+00' + make_interval(secs => n / 100),
    timestamptz '2026-01-01 00:00:00+00' + make_interval(secs => n / 2),
    true,
    CASE WHEN n % 3 = 0 THEN 'keycloak' END,
    n % 7 = 0
FROM generate_series(1, 500000) AS n;

CREATE TEMP TABLE perf_users_timestamp (LIKE perf_users_base INCLUDING ALL);
CREATE TEMP TABLE perf_users_compound (LIKE perf_users_base INCLUDING ALL);
INSERT INTO perf_users_timestamp SELECT * FROM perf_users_base;
INSERT INTO perf_users_compound SELECT * FROM perf_users_base;
CREATE INDEX perf_users_timestamp_idx ON perf_users_timestamp (created_at DESC);
CREATE INDEX perf_users_compound_idx ON perf_users_compound (created_at DESC, id DESC);
ANALYZE perf_users_base;
ANALYZE perf_users_timestamp;
ANALYZE perf_users_compound;

SELECT 'timestamp_index_bytes|' || pg_relation_size('perf_users_timestamp_idx');
SELECT 'compound_index_bytes|' || pg_relation_size('perf_users_compound_idx');
SELECT 'tied_timestamp_groups|' || COUNT(*)
FROM (SELECT created_at FROM perf_users_base GROUP BY created_at HAVING COUNT(*) > 1) tied;
SELECT 'all_orders_match|' || (
    ARRAY(SELECT id FROM perf_users_base ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
    = ARRAY(SELECT id FROM perf_users_timestamp ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
    AND ARRAY(SELECT id FROM perf_users_base ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
    = ARRAY(SELECT id FROM perf_users_compound ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
);

\o /dev/null
\timing on

\echo warmup_no_index_first
SELECT * FROM perf_users_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo warmup_timestamp_first
SELECT * FROM perf_users_timestamp ORDER BY created_at DESC, id DESC LIMIT 100;
\echo warmup_compound_first
SELECT * FROM perf_users_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo warmup_no_index_deep
SELECT * FROM perf_users_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo warmup_timestamp_deep
SELECT * FROM perf_users_timestamp ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo warmup_compound_deep
SELECT * FROM perf_users_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

-- Rotate execution order between samples.
\echo no_index_first_1
SELECT * FROM perf_users_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo timestamp_first_1
SELECT * FROM perf_users_timestamp ORDER BY created_at DESC, id DESC LIMIT 100;
\echo compound_first_1
SELECT * FROM perf_users_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo timestamp_deep_1
SELECT * FROM perf_users_timestamp ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo compound_deep_1
SELECT * FROM perf_users_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo no_index_deep_1
SELECT * FROM perf_users_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

\echo compound_first_2
SELECT * FROM perf_users_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo no_index_first_2
SELECT * FROM perf_users_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo timestamp_first_2
SELECT * FROM perf_users_timestamp ORDER BY created_at DESC, id DESC LIMIT 100;
\echo no_index_deep_2
SELECT * FROM perf_users_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo timestamp_deep_2
SELECT * FROM perf_users_timestamp ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo compound_deep_2
SELECT * FROM perf_users_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

\echo timestamp_first_3
SELECT * FROM perf_users_timestamp ORDER BY created_at DESC, id DESC LIMIT 100;
\echo compound_first_3
SELECT * FROM perf_users_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo no_index_first_3
SELECT * FROM perf_users_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo compound_deep_3
SELECT * FROM perf_users_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo no_index_deep_3
SELECT * FROM perf_users_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo timestamp_deep_3
SELECT * FROM perf_users_timestamp ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

\echo no_index_first_4
SELECT * FROM perf_users_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo compound_first_4
SELECT * FROM perf_users_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo timestamp_first_4
SELECT * FROM perf_users_timestamp ORDER BY created_at DESC, id DESC LIMIT 100;
\echo timestamp_deep_4
SELECT * FROM perf_users_timestamp ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo no_index_deep_4
SELECT * FROM perf_users_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo compound_deep_4
SELECT * FROM perf_users_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

\echo compound_first_5
SELECT * FROM perf_users_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo timestamp_first_5
SELECT * FROM perf_users_timestamp ORDER BY created_at DESC, id DESC LIMIT 100;
\echo no_index_first_5
SELECT * FROM perf_users_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo no_index_deep_5
SELECT * FROM perf_users_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo compound_deep_5
SELECT * FROM perf_users_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo timestamp_deep_5
SELECT * FROM perf_users_timestamp ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

-- Quantify both index write taxes over identical 10k-row inserts.
\echo no_index_insert_1
INSERT INTO perf_users_base SELECT * FROM perf_users_base LIMIT 10000;
\echo timestamp_insert_1
INSERT INTO perf_users_timestamp SELECT * FROM perf_users_timestamp LIMIT 10000;
\echo compound_insert_1
INSERT INTO perf_users_compound SELECT * FROM perf_users_compound LIMIT 10000;
\echo compound_insert_2
INSERT INTO perf_users_compound SELECT * FROM perf_users_compound LIMIT 10000;
\echo no_index_insert_2
INSERT INTO perf_users_base SELECT * FROM perf_users_base LIMIT 10000;
\echo timestamp_insert_2
INSERT INTO perf_users_timestamp SELECT * FROM perf_users_timestamp LIMIT 10000;
\echo timestamp_insert_3
INSERT INTO perf_users_timestamp SELECT * FROM perf_users_timestamp LIMIT 10000;
\echo compound_insert_3
INSERT INTO perf_users_compound SELECT * FROM perf_users_compound LIMIT 10000;
\echo no_index_insert_3
INSERT INTO perf_users_base SELECT * FROM perf_users_base LIMIT 10000;
\echo no_index_insert_4
INSERT INTO perf_users_base SELECT * FROM perf_users_base LIMIT 10000;
\echo timestamp_insert_4
INSERT INTO perf_users_timestamp SELECT * FROM perf_users_timestamp LIMIT 10000;
\echo compound_insert_4
INSERT INTO perf_users_compound SELECT * FROM perf_users_compound LIMIT 10000;
\echo compound_insert_5
INSERT INTO perf_users_compound SELECT * FROM perf_users_compound LIMIT 10000;
\echo timestamp_insert_5
INSERT INTO perf_users_timestamp SELECT * FROM perf_users_timestamp LIMIT 10000;
\echo no_index_insert_5
INSERT INTO perf_users_base SELECT * FROM perf_users_base LIMIT 10000;

\timing off
\o
ROLLBACK;
