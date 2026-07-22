\set ON_ERROR_STOP on
\pset pager off
\pset format unaligned
\pset tuples_only on

-- Isolated reproduction of auth.users' payload shape.  The transaction and
-- temporary table guarantee that the developer database is unchanged.
BEGIN;

CREATE TEMP TABLE perf_admin_users (
    id uuid NOT NULL,
    username text,
    email text NOT NULL,
    password_hash text NOT NULL,
    role_text text NOT NULL,
    storage_quota_bytes bigint NOT NULL,
    storage_used_bytes bigint NOT NULL,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    last_login_at timestamptz,
    active boolean NOT NULL,
    oidc_provider text,
    oidc_subject text,
    image text,
    is_external boolean NOT NULL,
    given_name text,
    family_name text,
    email_verified_at timestamptz,
    preferred_locale text,
    notify_on_share boolean NOT NULL,
    ui_preferences jsonb NOT NULL
);

-- One incompressible-ish 512 KiB base64/data-URI-shaped avatar and an 8 KiB
-- JSON preference bag per row.  This is the documented maximum avatar size and
-- intentionally models the expensive end of the admin endpoint.
WITH payload AS (
    SELECT string_agg(md5(i::text || ':oxicloud-perf'), '') AS random_hex
    FROM generate_series(1, 16384) AS i
)
INSERT INTO perf_admin_users
SELECT
    gen_random_uuid(),
    'perf-user-' || n,
    'perf-user-' || n || '@example.invalid',
    '$argon2id$v=19$m=19456,t=2,p=1$benchmark-only',
    CASE WHEN n % 20 = 0 THEN 'admin' ELSE 'user' END,
    10737418240,
    (n * 1048576)::bigint,
    clock_timestamp() - make_interval(secs => n),
    clock_timestamp(),
    clock_timestamp() - make_interval(mins => n),
    true,
    CASE WHEN n % 3 = 0 THEN 'keycloak' END,
    CASE WHEN n % 3 = 0 THEN 'subject-' || n END,
    'data:image/webp;base64,' || payload.random_hex,
    false,
    'Given' || n,
    'Family' || n,
    clock_timestamp(),
    'es',
    true,
    jsonb_build_object('perf_blob', left(payload.random_hex, 8192))
FROM generate_series(1, 100) AS n
CROSS JOIN payload;

ANALYZE perf_admin_users;

-- Bytes serialized by the current endpoint versus the proposed summary DTO.
-- These include exactly the JSON fields each HTTP response shape emits.
SELECT 'current_json_bytes|' || sum(octet_length(jsonb_build_object(
    'id', id::text,
    'username', username,
    'email', email,
    'role', role_text,
    'storage_quota_bytes', storage_quota_bytes,
    'storage_used_bytes', storage_used_bytes,
    'created_at', created_at,
    'updated_at', updated_at,
    'last_login_at', last_login_at,
    'active', active,
    'auth_provider', coalesce(oidc_provider, 'local'),
    'image', image,
    'can_edit_image', oidc_provider IS NULL,
    'is_external', is_external,
    'given_name', given_name,
    'family_name', family_name,
    'email_verified_at', email_verified_at,
    'preferred_locale', preferred_locale,
    'notify_on_share', notify_on_share,
    'ui_preferences', ui_preferences
)::text))
FROM perf_admin_users;

SELECT 'summary_json_bytes|' || sum(octet_length(jsonb_build_object(
    'id', id::text,
    'username', username,
    'email', email,
    'role', role_text,
    'storage_quota_bytes', storage_quota_bytes,
    'storage_used_bytes', storage_used_bytes,
    'last_login_at', last_login_at,
    'active', active,
    'auth_provider', coalesce(oidc_provider, 'local'),
    'is_external', is_external
)::text))
FROM perf_admin_users;

-- psql's timer covers server execution, transfer and client decoding. Query
-- output goes to /dev/null so terminal rendering does not dominate the result.
\o /dev/null
\timing on

\echo current_warmup
SELECT id, username, email, password_hash, role_text,
       storage_quota_bytes, storage_used_bytes, created_at, updated_at,
       last_login_at, active, oidc_provider, oidc_subject, image, is_external,
       given_name, family_name, email_verified_at, preferred_locale,
       notify_on_share, ui_preferences
FROM perf_admin_users ORDER BY created_at DESC LIMIT 100 OFFSET 0;

\echo summary_warmup
SELECT id, username, email, role_text, storage_quota_bytes,
       storage_used_bytes, last_login_at, active, oidc_provider, is_external
FROM perf_admin_users ORDER BY created_at DESC LIMIT 100 OFFSET 0;

\echo current_sample_1
SELECT id, username, email, password_hash, role_text,
       storage_quota_bytes, storage_used_bytes, created_at, updated_at,
       last_login_at, active, oidc_provider, oidc_subject, image, is_external,
       given_name, family_name, email_verified_at, preferred_locale,
       notify_on_share, ui_preferences
FROM perf_admin_users ORDER BY created_at DESC LIMIT 100 OFFSET 0;
\echo summary_sample_1
SELECT id, username, email, role_text, storage_quota_bytes,
       storage_used_bytes, last_login_at, active, oidc_provider, is_external
FROM perf_admin_users ORDER BY created_at DESC LIMIT 100 OFFSET 0;

\echo current_sample_2
SELECT id, username, email, password_hash, role_text,
       storage_quota_bytes, storage_used_bytes, created_at, updated_at,
       last_login_at, active, oidc_provider, oidc_subject, image, is_external,
       given_name, family_name, email_verified_at, preferred_locale,
       notify_on_share, ui_preferences
FROM perf_admin_users ORDER BY created_at DESC LIMIT 100 OFFSET 0;
\echo summary_sample_2
SELECT id, username, email, role_text, storage_quota_bytes,
       storage_used_bytes, last_login_at, active, oidc_provider, is_external
FROM perf_admin_users ORDER BY created_at DESC LIMIT 100 OFFSET 0;

\echo current_sample_3
SELECT id, username, email, password_hash, role_text,
       storage_quota_bytes, storage_used_bytes, created_at, updated_at,
       last_login_at, active, oidc_provider, oidc_subject, image, is_external,
       given_name, family_name, email_verified_at, preferred_locale,
       notify_on_share, ui_preferences
FROM perf_admin_users ORDER BY created_at DESC LIMIT 100 OFFSET 0;
\echo summary_sample_3
SELECT id, username, email, role_text, storage_quota_bytes,
       storage_used_bytes, last_login_at, active, oidc_provider, is_external
FROM perf_admin_users ORDER BY created_at DESC LIMIT 100 OFFSET 0;

\echo current_sample_4
SELECT id, username, email, password_hash, role_text,
       storage_quota_bytes, storage_used_bytes, created_at, updated_at,
       last_login_at, active, oidc_provider, oidc_subject, image, is_external,
       given_name, family_name, email_verified_at, preferred_locale,
       notify_on_share, ui_preferences
FROM perf_admin_users ORDER BY created_at DESC LIMIT 100 OFFSET 0;
\echo summary_sample_4
SELECT id, username, email, role_text, storage_quota_bytes,
       storage_used_bytes, last_login_at, active, oidc_provider, is_external
FROM perf_admin_users ORDER BY created_at DESC LIMIT 100 OFFSET 0;

\echo current_sample_5
SELECT id, username, email, password_hash, role_text,
       storage_quota_bytes, storage_used_bytes, created_at, updated_at,
       last_login_at, active, oidc_provider, oidc_subject, image, is_external,
       given_name, family_name, email_verified_at, preferred_locale,
       notify_on_share, ui_preferences
FROM perf_admin_users ORDER BY created_at DESC LIMIT 100 OFFSET 0;
\echo summary_sample_5
SELECT id, username, email, role_text, storage_quota_bytes,
       storage_used_bytes, last_login_at, active, oidc_provider, is_external
FROM perf_admin_users ORDER BY created_at DESC LIMIT 100 OFFSET 0;

\timing off
\o
ROLLBACK;
