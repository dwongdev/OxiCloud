\set ON_ERROR_STOP on
\pset pager off
\pset format unaligned
\pset tuples_only on

-- Representative A/B/C gate for admin-list indexes:
--   A. no ordering index;
--   B. created_at DESC (the accepted narrow production variant);
--   C. created_at DESC, id DESC (the rejected compound candidate).
--
-- The original Pareto fixture intentionally put 100 users under every
-- timestamp to stress the incremental id sort. PostgreSQL can compress those
-- duplicate B-tree keys into posting lists, however, so that fixture may
-- materially understate index bytes and insert cost for normal registrations.
-- This gate keeps the same primary-key index on every A/B/C table and covers:
--   * unique timestamps (one normal registration per transaction), and
--   * ten-user bursts (small provisioning/import transactions).
-- New insert batches use new timestamps instead of duplicating old keys.
BEGIN;

CREATE TEMP TABLE perf_unique_base (
    id uuid PRIMARY KEY,
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
CREATE TEMP TABLE perf_unique_indexed (LIKE perf_unique_base INCLUDING ALL);
CREATE TEMP TABLE perf_unique_compound (LIKE perf_unique_base INCLUDING ALL);

INSERT INTO perf_unique_base
SELECT
    gen_random_uuid(),
    'unique-user-' || n,
    'unique-user-' || n || '@example.invalid',
    CASE WHEN n % 20 = 0 THEN 'admin' ELSE 'user' END,
    10737418240,
    n::bigint * 1048576,
    timestamptz '2026-01-01 00:00:00+00' + n * interval '1 microsecond',
    timestamptz '2026-01-01 00:00:00+00' + n * interval '1 second',
    true,
    CASE WHEN n % 3 = 0 THEN 'keycloak' END,
    n % 7 = 0
FROM generate_series(1, 500000) AS n;
INSERT INTO perf_unique_indexed SELECT * FROM perf_unique_base;
INSERT INTO perf_unique_compound SELECT * FROM perf_unique_base;
CREATE INDEX perf_unique_created_at_idx
    ON perf_unique_indexed (created_at DESC);
CREATE INDEX perf_unique_created_at_id_idx
    ON perf_unique_compound (created_at DESC, id DESC);

CREATE TEMP TABLE perf_burst_base (LIKE perf_unique_base INCLUDING ALL);
CREATE TEMP TABLE perf_burst_indexed (LIKE perf_unique_base INCLUDING ALL);
CREATE TEMP TABLE perf_burst_compound (LIKE perf_unique_base INCLUDING ALL);
INSERT INTO perf_burst_base
SELECT
    gen_random_uuid(),
    'burst-user-' || n,
    'burst-user-' || n || '@example.invalid',
    CASE WHEN n % 20 = 0 THEN 'admin' ELSE 'user' END,
    10737418240,
    n::bigint * 1048576,
    timestamptz '2026-01-01 00:00:00+00'
        + ((n - 1) / 10) * interval '1 millisecond',
    timestamptz '2026-01-01 00:00:00+00' + n * interval '1 second',
    true,
    CASE WHEN n % 3 = 0 THEN 'keycloak' END,
    n % 7 = 0
FROM generate_series(1, 500000) AS n;
INSERT INTO perf_burst_indexed SELECT * FROM perf_burst_base;
INSERT INTO perf_burst_compound SELECT * FROM perf_burst_base;
CREATE INDEX perf_burst_created_at_idx
    ON perf_burst_indexed (created_at DESC);
CREATE INDEX perf_burst_created_at_id_idx
    ON perf_burst_compound (created_at DESC, id DESC);

-- Pre-build deterministic new-row batches. All A/B/C tables receive identical
-- values; the indexed sample column keeps batch-selection work bounded/common.
CREATE TEMP TABLE perf_unique_insert_rows AS
SELECT
    sample,
    md5('perf-unique-' || sample || '-' || n)::uuid AS id,
    'new-unique-' || sample || '-' || n AS username,
    'new-unique-' || sample || '-' || n || '@example.invalid' AS email,
    'user'::text AS role_text,
    10737418240::bigint AS storage_quota_bytes,
    n::bigint * 1048576 AS storage_used_bytes,
    timestamptz '2027-01-01 00:00:00+00'
        + ((sample - 1) * 10000 + n) * interval '1 microsecond' AS created_at,
    NULL::timestamptz AS last_login_at,
    true AS active,
    NULL::text AS oidc_provider,
    false AS is_external
FROM generate_series(1, 5) AS sample
CROSS JOIN generate_series(1, 10000) AS n;
CREATE INDEX perf_unique_insert_sample_idx ON perf_unique_insert_rows (sample);

CREATE TEMP TABLE perf_burst_insert_rows AS
SELECT
    sample,
    md5('perf-burst-' || sample || '-' || n)::uuid AS id,
    'new-burst-' || sample || '-' || n AS username,
    'new-burst-' || sample || '-' || n || '@example.invalid' AS email,
    'user'::text AS role_text,
    10737418240::bigint AS storage_quota_bytes,
    n::bigint * 1048576 AS storage_used_bytes,
    timestamptz '2027-01-01 00:00:00+00'
        + (((sample - 1) * 10000 + n - 1) / 10) * interval '1 millisecond'
        AS created_at,
    NULL::timestamptz AS last_login_at,
    true AS active,
    NULL::text AS oidc_provider,
    false AS is_external
FROM generate_series(1, 5) AS sample
CROSS JOIN generate_series(1, 10000) AS n;
CREATE INDEX perf_burst_insert_sample_idx ON perf_burst_insert_rows (sample);

ANALYZE perf_unique_base;
ANALYZE perf_unique_indexed;
ANALYZE perf_unique_compound;
ANALYZE perf_burst_base;
ANALYZE perf_burst_indexed;
ANALYZE perf_burst_compound;
ANALYZE perf_unique_insert_rows;
ANALYZE perf_burst_insert_rows;

SELECT 'unique_index_bytes|' || pg_relation_size('perf_unique_created_at_idx');
SELECT 'unique_compound_index_bytes|'
    || pg_relation_size('perf_unique_created_at_id_idx');
SELECT 'burst10_index_bytes|' || pg_relation_size('perf_burst_created_at_idx');
SELECT 'burst10_compound_index_bytes|'
    || pg_relation_size('perf_burst_created_at_id_idx');
SELECT 'unique_distinct_timestamps|' || COUNT(DISTINCT created_at)
FROM perf_unique_base;
SELECT 'burst10_distinct_timestamps|' || COUNT(DISTINCT created_at)
FROM perf_burst_base;
SELECT 'unique_order_match|' || (
    ARRAY(SELECT id FROM perf_unique_base
          ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
    = ARRAY(SELECT id FROM perf_unique_indexed
            ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
    AND ARRAY(SELECT id FROM perf_unique_base
              ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
        = ARRAY(SELECT id FROM perf_unique_compound
                ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
);
SELECT 'burst10_order_match|' || (
    ARRAY(SELECT id FROM perf_burst_base
          ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
    = ARRAY(SELECT id FROM perf_burst_indexed
            ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
    AND ARRAY(SELECT id FROM perf_burst_base
              ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
        = ARRAY(SELECT id FROM perf_burst_compound
                ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
);

\o /dev/null
\timing on

-- Warmups.
\echo unique_no_index_first_warmup
SELECT * FROM perf_unique_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_index_first_warmup
SELECT * FROM perf_unique_indexed ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_compound_first_warmup
SELECT * FROM perf_unique_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_no_index_deep_warmup
SELECT * FROM perf_unique_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo unique_index_deep_warmup
SELECT * FROM perf_unique_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo unique_compound_deep_warmup
SELECT * FROM perf_unique_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_no_index_first_warmup
SELECT * FROM perf_burst_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_index_first_warmup
SELECT * FROM perf_burst_indexed ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_compound_first_warmup
SELECT * FROM perf_burst_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_no_index_deep_warmup
SELECT * FROM perf_burst_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_index_deep_warmup
SELECT * FROM perf_burst_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_compound_deep_warmup
SELECT * FROM perf_burst_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

-- Five samples per read shape, with A/B/C order rotated.
\echo unique_no_index_first_1
SELECT * FROM perf_unique_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_index_first_1
SELECT * FROM perf_unique_indexed ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_compound_first_1
SELECT * FROM perf_unique_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_index_deep_1
SELECT * FROM perf_unique_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo unique_compound_deep_1
SELECT * FROM perf_unique_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo unique_no_index_deep_1
SELECT * FROM perf_unique_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_no_index_first_1
SELECT * FROM perf_burst_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_index_first_1
SELECT * FROM perf_burst_indexed ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_compound_first_1
SELECT * FROM perf_burst_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_index_deep_1
SELECT * FROM perf_burst_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_compound_deep_1
SELECT * FROM perf_burst_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_no_index_deep_1
SELECT * FROM perf_burst_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

\echo unique_compound_first_2
SELECT * FROM perf_unique_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_index_first_2
SELECT * FROM perf_unique_indexed ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_no_index_first_2
SELECT * FROM perf_unique_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_no_index_deep_2
SELECT * FROM perf_unique_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo unique_index_deep_2
SELECT * FROM perf_unique_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo unique_compound_deep_2
SELECT * FROM perf_unique_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_compound_first_2
SELECT * FROM perf_burst_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_index_first_2
SELECT * FROM perf_burst_indexed ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_no_index_first_2
SELECT * FROM perf_burst_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_no_index_deep_2
SELECT * FROM perf_burst_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_index_deep_2
SELECT * FROM perf_burst_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_compound_deep_2
SELECT * FROM perf_burst_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

\echo unique_compound_first_3
SELECT * FROM perf_unique_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_no_index_first_3
SELECT * FROM perf_unique_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_index_first_3
SELECT * FROM perf_unique_indexed ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_no_index_deep_3
SELECT * FROM perf_unique_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo unique_index_deep_3
SELECT * FROM perf_unique_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo unique_compound_deep_3
SELECT * FROM perf_unique_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_compound_first_3
SELECT * FROM perf_burst_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_no_index_first_3
SELECT * FROM perf_burst_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_index_first_3
SELECT * FROM perf_burst_indexed ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_no_index_deep_3
SELECT * FROM perf_burst_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_index_deep_3
SELECT * FROM perf_burst_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_compound_deep_3
SELECT * FROM perf_burst_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

\echo unique_index_first_4
SELECT * FROM perf_unique_indexed ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_compound_first_4
SELECT * FROM perf_unique_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_no_index_first_4
SELECT * FROM perf_unique_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_index_deep_4
SELECT * FROM perf_unique_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo unique_compound_deep_4
SELECT * FROM perf_unique_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo unique_no_index_deep_4
SELECT * FROM perf_unique_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_index_first_4
SELECT * FROM perf_burst_indexed ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_compound_first_4
SELECT * FROM perf_burst_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_no_index_first_4
SELECT * FROM perf_burst_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_index_deep_4
SELECT * FROM perf_burst_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_compound_deep_4
SELECT * FROM perf_burst_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_no_index_deep_4
SELECT * FROM perf_burst_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

\echo unique_no_index_first_5
SELECT * FROM perf_unique_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_compound_first_5
SELECT * FROM perf_unique_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_index_first_5
SELECT * FROM perf_unique_indexed ORDER BY created_at DESC, id DESC LIMIT 100;
\echo unique_index_deep_5
SELECT * FROM perf_unique_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo unique_compound_deep_5
SELECT * FROM perf_unique_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo unique_no_index_deep_5
SELECT * FROM perf_unique_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_no_index_first_5
SELECT * FROM perf_burst_base ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_compound_first_5
SELECT * FROM perf_burst_compound ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_index_first_5
SELECT * FROM perf_burst_indexed ORDER BY created_at DESC, id DESC LIMIT 100;
\echo burst10_index_deep_5
SELECT * FROM perf_burst_indexed ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_compound_deep_5
SELECT * FROM perf_burst_compound ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;
\echo burst10_no_index_deep_5
SELECT * FROM perf_burst_base ORDER BY created_at DESC, id DESC LIMIT 100 OFFSET 50000;

-- Five 10k-row inserts per distribution. Rotate A/B/C order.
\echo unique_no_index_insert_1
INSERT INTO perf_unique_base SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 1;
\echo unique_index_insert_1
INSERT INTO perf_unique_indexed SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 1;
\echo unique_compound_insert_1
INSERT INTO perf_unique_compound SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 1;
\echo burst10_no_index_insert_1
INSERT INTO perf_burst_base SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 1;
\echo burst10_index_insert_1
INSERT INTO perf_burst_indexed SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 1;
\echo burst10_compound_insert_1
INSERT INTO perf_burst_compound SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 1;

\echo unique_compound_insert_2
INSERT INTO perf_unique_compound SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 2;
\echo unique_index_insert_2
INSERT INTO perf_unique_indexed SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 2;
\echo unique_no_index_insert_2
INSERT INTO perf_unique_base SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 2;
\echo burst10_compound_insert_2
INSERT INTO perf_burst_compound SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 2;
\echo burst10_index_insert_2
INSERT INTO perf_burst_indexed SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 2;
\echo burst10_no_index_insert_2
INSERT INTO perf_burst_base SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 2;

\echo unique_compound_insert_3
INSERT INTO perf_unique_compound SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 3;
\echo unique_no_index_insert_3
INSERT INTO perf_unique_base SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 3;
\echo unique_index_insert_3
INSERT INTO perf_unique_indexed SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 3;
\echo burst10_compound_insert_3
INSERT INTO perf_burst_compound SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 3;
\echo burst10_no_index_insert_3
INSERT INTO perf_burst_base SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 3;
\echo burst10_index_insert_3
INSERT INTO perf_burst_indexed SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 3;

\echo unique_index_insert_4
INSERT INTO perf_unique_indexed SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 4;
\echo unique_compound_insert_4
INSERT INTO perf_unique_compound SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 4;
\echo unique_no_index_insert_4
INSERT INTO perf_unique_base SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 4;
\echo burst10_index_insert_4
INSERT INTO perf_burst_indexed SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 4;
\echo burst10_compound_insert_4
INSERT INTO perf_burst_compound SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 4;
\echo burst10_no_index_insert_4
INSERT INTO perf_burst_base SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 4;

\echo unique_no_index_insert_5
INSERT INTO perf_unique_base SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 5;
\echo unique_compound_insert_5
INSERT INTO perf_unique_compound SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 5;
\echo unique_index_insert_5
INSERT INTO perf_unique_indexed SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_unique_insert_rows WHERE sample = 5;
\echo burst10_no_index_insert_5
INSERT INTO perf_burst_base SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 5;
\echo burst10_compound_insert_5
INSERT INTO perf_burst_compound SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 5;
\echo burst10_index_insert_5
INSERT INTO perf_burst_indexed SELECT id, username, email, role_text,
    storage_quota_bytes, storage_used_bytes, created_at, last_login_at,
    active, oidc_provider, is_external
FROM perf_burst_insert_rows WHERE sample = 5;

\timing off
\o

SELECT 'unique_final_count_match|' || (
    (SELECT COUNT(*) FROM perf_unique_base)
    = (SELECT COUNT(*) FROM perf_unique_indexed)
    AND (SELECT COUNT(*) FROM perf_unique_base)
        = (SELECT COUNT(*) FROM perf_unique_compound)
);
SELECT 'burst10_final_count_match|' || (
    (SELECT COUNT(*) FROM perf_burst_base)
    = (SELECT COUNT(*) FROM perf_burst_indexed)
    AND (SELECT COUNT(*) FROM perf_burst_base)
        = (SELECT COUNT(*) FROM perf_burst_compound)
);
SELECT 'unique_final_order_match|' || (
    ARRAY(SELECT id FROM perf_unique_base
          ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
    = ARRAY(SELECT id FROM perf_unique_indexed
            ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
    AND ARRAY(SELECT id FROM perf_unique_base
              ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
        = ARRAY(SELECT id FROM perf_unique_compound
                ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
);
SELECT 'burst10_final_order_match|' || (
    ARRAY(SELECT id FROM perf_burst_base
          ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
    = ARRAY(SELECT id FROM perf_burst_indexed
            ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
    AND ARRAY(SELECT id FROM perf_burst_base
              ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
        = ARRAY(SELECT id FROM perf_burst_compound
                ORDER BY created_at DESC, id DESC LIMIT 200 OFFSET 49900)
);
SELECT 'unique_index_bytes_after_50k_inserts|'
    || pg_relation_size('perf_unique_created_at_idx');
SELECT 'unique_compound_index_bytes_after_50k_inserts|'
    || pg_relation_size('perf_unique_created_at_id_idx');
SELECT 'burst10_index_bytes_after_50k_inserts|'
    || pg_relation_size('perf_burst_created_at_idx');
SELECT 'burst10_compound_index_bytes_after_50k_inserts|'
    || pg_relation_size('perf_burst_created_at_id_idx');

ROLLBACK;
