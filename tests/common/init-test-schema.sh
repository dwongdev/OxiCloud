#!/usr/bin/env bash
# Apply every migration in lexical order to a test database, then seed
# the minimum `auth.users` row that integration tests need.
#
# Connection parameters come from the libpq env vars (PGHOST, PGPORT,
# PGUSER, PGPASSWORD, PGDATABASE) so the same script works against:
#
#   - the local docker-compose-test postgres on port 5433
#     (PGHOST=localhost PGPORT=5433 PGUSER=oxicloud_test
#      PGPASSWORD=oxicloud_test PGDATABASE=oxicloud_test)
#
#   - the CI postgres service on port 5432
#     (PGHOST=localhost PGPORT=5432 PGUSER=postgres
#      PGPASSWORD=postgres PGDATABASE=oxicloud_test)
#
# The seed user is purely a placeholder so `first_admin()` in the Rust
# integration tests has a UUID to attach `added_by` to. The password
# hash is not a real argon2 hash — these tests never log in as this
# user, only reference its id.

set -euo pipefail

: "${PGHOST:?PGHOST must be set}"
: "${PGPORT:?PGPORT must be set}"
: "${PGUSER:?PGUSER must be set}"
: "${PGPASSWORD:?PGPASSWORD must be set}"
: "${PGDATABASE:?PGDATABASE must be set}"
export PGHOST PGPORT PGUSER PGPASSWORD PGDATABASE

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

echo "[init-schema] applying migrations to ${PGUSER}@${PGHOST}:${PGPORT}/${PGDATABASE}"
for f in "$REPO_ROOT"/migrations/*.sql; do
    echo "[init-schema]   $(basename "$f")"
    psql -v ON_ERROR_STOP=1 -f "$f" >/dev/null
done

echo "[init-schema] seeding ci-admin row (idempotent)"
psql -v ON_ERROR_STOP=1 -c "
    INSERT INTO auth.users (username, email, password_hash, role)
    VALUES ('ci-admin', 'ci-admin@example.test', 'placeholder-not-validated', 'admin')
    ON CONFLICT (username) DO NOTHING;
" >/dev/null

# The OxiCloud server normally provisions a default Personal drive +
# Owner role_grant on user creation via PersonalDriveLifecycleHook
# (D0). This script bypasses that pipeline — it INSERTs directly into
# auth.users — so we mirror the hook's behaviour here. Without it,
# integration test fixtures that hand-roll INSERTs into storage.files
# fail with "drive_id not-null violation" (M3 made the column
# mandatory), and helpers that JOIN auth.users with storage.drives
# return RowNotFound.
echo "[init-schema] provisioning ci-admin's default Personal drive (idempotent)"
psql -v ON_ERROR_STOP=1 <<'SQL' >/dev/null
WITH admin AS (
    SELECT id FROM auth.users WHERE username = 'ci-admin'
),
ins_drive AS (
    INSERT INTO storage.drives (name, kind, default_for_user, quota_bytes)
    SELECT 'Personal', 'personal', admin.id, NULL
      FROM admin
     WHERE NOT EXISTS (
         SELECT 1 FROM storage.drives d WHERE d.default_for_user = admin.id
     )
    RETURNING id, default_for_user
)
INSERT INTO storage.role_grants
    (subject_type, subject_id, resource_type, resource_id, role, granted_by)
SELECT 'user', ins_drive.default_for_user, 'drive', ins_drive.id, 'owner',
       ins_drive.default_for_user
  FROM ins_drive
 WHERE NOT EXISTS (
     SELECT 1 FROM storage.role_grants g
      WHERE g.subject_type   = 'user'
        AND g.subject_id     = ins_drive.default_for_user
        AND g.resource_type  = 'drive'
        AND g.resource_id    = ins_drive.id
 );
SQL

echo "[init-schema] done"
