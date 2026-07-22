\set ON_ERROR_STOP on
\pset pager off
\pset format unaligned
\pset tuples_only on

-- Compare the endpoint's narrow page + independent count with a tempting
-- COUNT(*) OVER() fusion.  The transaction/temp table leave no persistent
-- database state.  This benchmark exists to reject the fusion if the window
-- forces PostgreSQL to materialise too much of a large directory.
BEGIN;

CREATE TEMP TABLE perf_admin_count (
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

INSERT INTO perf_admin_count
SELECT
    gen_random_uuid(),
    'user-' || n,
    'user-' || n || '@example.invalid',
    CASE WHEN n % 20 = 0 THEN 'admin' ELSE 'user' END,
    10737418240,
    n::bigint * 1048576,
    clock_timestamp() - make_interval(secs => n),
    clock_timestamp() - make_interval(mins => n),
    true,
    CASE WHEN n % 3 = 0 THEN 'keycloak' END,
    n % 7 = 0
FROM generate_series(1, 100000) AS n;

CREATE INDEX perf_admin_count_created_idx
    ON perf_admin_count (created_at DESC);
ANALYZE perf_admin_count;

\o /dev/null
\timing on

-- A sample consists of these two statements; add their reported times.
\echo current_warmup_page
SELECT id, username, email, role_text, storage_quota_bytes,
       storage_used_bytes, last_login_at, active, oidc_provider, is_external
FROM perf_admin_count
ORDER BY created_at DESC LIMIT 100 OFFSET 0;
\echo current_warmup_count
SELECT COUNT(*) FROM perf_admin_count;

\echo candidate_warmup
SELECT id, username, email, role_text, storage_quota_bytes,
       storage_used_bytes, last_login_at, active, oidc_provider, is_external,
       COUNT(*) OVER () AS total
FROM perf_admin_count
ORDER BY created_at DESC LIMIT 100 OFFSET 0;

\echo current_1_page
SELECT id, username, email, role_text, storage_quota_bytes, storage_used_bytes,
       last_login_at, active, oidc_provider, is_external
FROM perf_admin_count ORDER BY created_at DESC LIMIT 100;
\echo current_1_count
SELECT COUNT(*) FROM perf_admin_count;
\echo candidate_1
SELECT id, username, email, role_text, storage_quota_bytes, storage_used_bytes,
       last_login_at, active, oidc_provider, is_external, COUNT(*) OVER () AS total
FROM perf_admin_count ORDER BY created_at DESC LIMIT 100;

\echo candidate_2
SELECT id, username, email, role_text, storage_quota_bytes, storage_used_bytes,
       last_login_at, active, oidc_provider, is_external, COUNT(*) OVER () AS total
FROM perf_admin_count ORDER BY created_at DESC LIMIT 100;
\echo current_2_page
SELECT id, username, email, role_text, storage_quota_bytes, storage_used_bytes,
       last_login_at, active, oidc_provider, is_external
FROM perf_admin_count ORDER BY created_at DESC LIMIT 100;
\echo current_2_count
SELECT COUNT(*) FROM perf_admin_count;

\echo current_3_page
SELECT id, username, email, role_text, storage_quota_bytes, storage_used_bytes,
       last_login_at, active, oidc_provider, is_external
FROM perf_admin_count ORDER BY created_at DESC LIMIT 100;
\echo current_3_count
SELECT COUNT(*) FROM perf_admin_count;
\echo candidate_3
SELECT id, username, email, role_text, storage_quota_bytes, storage_used_bytes,
       last_login_at, active, oidc_provider, is_external, COUNT(*) OVER () AS total
FROM perf_admin_count ORDER BY created_at DESC LIMIT 100;

\echo candidate_4
SELECT id, username, email, role_text, storage_quota_bytes, storage_used_bytes,
       last_login_at, active, oidc_provider, is_external, COUNT(*) OVER () AS total
FROM perf_admin_count ORDER BY created_at DESC LIMIT 100;
\echo current_4_page
SELECT id, username, email, role_text, storage_quota_bytes, storage_used_bytes,
       last_login_at, active, oidc_provider, is_external
FROM perf_admin_count ORDER BY created_at DESC LIMIT 100;
\echo current_4_count
SELECT COUNT(*) FROM perf_admin_count;

\echo current_5_page
SELECT id, username, email, role_text, storage_quota_bytes, storage_used_bytes,
       last_login_at, active, oidc_provider, is_external
FROM perf_admin_count ORDER BY created_at DESC LIMIT 100;
\echo current_5_count
SELECT COUNT(*) FROM perf_admin_count;
\echo candidate_5
SELECT id, username, email, role_text, storage_quota_bytes, storage_used_bytes,
       last_login_at, active, oidc_provider, is_external, COUNT(*) OVER () AS total
FROM perf_admin_count ORDER BY created_at DESC LIMIT 100;

\timing off
\o
ROLLBACK;
