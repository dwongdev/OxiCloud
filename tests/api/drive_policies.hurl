# =============================================================
# OxiCloud – D5 drive policies: `forbid_public_links`
# =============================================================
# Run:
#   hurl --variables-file tests/api/test.env --file-root tests \
#        --test tests/api/drive_policies.hurl
#
# The model under test (`docs/plan/drive.md` §8):
#   Each drive carries a `policies` JSONB. Five known keys, all
#   default-false. The first key shipped is `forbid_public_links`,
#   which blocks anonymous token-share creation on every resource
#   in the drive. Enforced at `share_service::create_shared_link`;
#   mutated by `PATCH /api/drives/{id}/policies` (Owner-only,
#   per the §4 role bundle).
#
# Cases:
#   1. Baseline — policy off → POST /api/shares succeeds (201).
#   2. Owner flips `forbid_public_links` via PATCH → 200,
#      response echoes the merged bag.
#   3. Policy on → POST /api/shares refused with
#      OperationNotSupported (405) and the share row is NOT created.
#   4. Owner flips the policy back off → POST /api/shares succeeds
#      again (proves merge semantics; the typed write doesn't
#      clobber unrelated keys).
#
# Self-contained: provisions `dp_owner` so it can run alongside
# the rest of the suite. The user's default Personal drive is
# the test surface — the policy applies equally to personal and
# shared drives (`Owner` bundle includes "edit policies").
# =============================================================


# ─────────────────────────────────────────────────────────────
# Step 1 — Admin login.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/auth/login
Content-Type: application/json
{ "username": "{{username}}", "password": "{{password}}" }

HTTP 200
[Captures]
admin_token: jsonpath "$.access_token"


# ─────────────────────────────────────────────────────────────
# Step 2 — Provision `dp_owner`.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/admin/users
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "username": "dp_owner",
  "password": "DpOwnerPwd1!",
  "email":    "dp_owner@example.com",
  "role":     "user"
}

HTTP 201

POST {{base_url}}/api/auth/login
Content-Type: application/json
{ "username": "dp_owner", "password": "DpOwnerPwd1!" }

HTTP 200
[Captures]
owner_token:   jsonpath "$.access_token"
owner_user_id: jsonpath "$.user.id"


# ─────────────────────────────────────────────────────────────
# Step 3 — Find the user's default Personal drive + root.
# ─────────────────────────────────────────────────────────────
GET {{base_url}}/api/folders
Authorization: Bearer {{owner_token}}

HTTP 200
[Captures]
personal_root_id: jsonpath "$[0].id"

GET {{base_url}}/api/drives
Authorization: Bearer {{owner_token}}

HTTP 200
[Captures]
personal_drive_id: jsonpath "$[0].id"
[Asserts]
jsonpath "$[0].kind" == "personal"


# ─────────────────────────────────────────────────────────────
# Step 4 — Seed a file to share.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/files/upload
Authorization: Bearer {{owner_token}}
[MultipartFormData]
folder_id: {{personal_root_id}}
file: file,fixtures/hello.txt; text/plain

HTTP 201
[Captures]
file_id: jsonpath "$.id"


# ─────────────────────────────────────────────────────────────
# Step 5 — Case 1: baseline. Policy off → POST /api/shares OK.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/shares
Authorization: Bearer {{owner_token}}
Content-Type: application/json
{
  "item_id":   "{{file_id}}",
  "item_type": "file"
}

HTTP 201
[Captures]
baseline_share_id: jsonpath "$.id"


# Clean up the baseline share so the policy-on case starts fresh.
DELETE {{base_url}}/api/shares/{{baseline_share_id}}
Authorization: Bearer {{owner_token}}

HTTP 204


# ─────────────────────────────────────────────────────────────
# Step 6 — Case 2: flip `forbid_public_links` on.
#          PATCH returns the merged bag.
# ─────────────────────────────────────────────────────────────
PATCH {{base_url}}/api/drives/{{personal_drive_id}}/policies
Authorization: Bearer {{owner_token}}
Content-Type: application/json
{
  "forbid_public_links": true
}

HTTP 200
[Asserts]
jsonpath "$.forbid_public_links"        == true
jsonpath "$.forbid_sharing"             == false
jsonpath "$.forbid_external_sharing"    == false
jsonpath "$.forbid_cross_drive_move"    == false


# ─────────────────────────────────────────────────────────────
# Step 7 — Case 3: policy on → POST /api/shares refused (405).
#          DomainError::operation_not_supported maps to HTTP 405
#          (Method Not Allowed) per the interface error map.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/shares
Authorization: Bearer {{owner_token}}
Content-Type: application/json
{
  "item_id":   "{{file_id}}",
  "item_type": "file"
}

HTTP 405


# Confirm no share row was created — the listing on this file
# is empty.
GET {{base_url}}/api/shares?item_id={{file_id}}&item_type=file
Authorization: Bearer {{owner_token}}

HTTP 200
[Asserts]
jsonpath "$" count == 0


# ─────────────────────────────────────────────────────────────
# Step 8 — Case 4: flip the policy back off → share succeeds.
#          Proves the partial-merge: setting `forbid_public_links`
#          to false doesn't touch unrelated keys (still false here,
#          but the round-trip exercises the merge path).
# ─────────────────────────────────────────────────────────────
PATCH {{base_url}}/api/drives/{{personal_drive_id}}/policies
Authorization: Bearer {{owner_token}}
Content-Type: application/json
{
  "forbid_public_links": false
}

HTTP 200
[Asserts]
jsonpath "$.forbid_public_links" == false


POST {{base_url}}/api/shares
Authorization: Bearer {{owner_token}}
Content-Type: application/json
{
  "item_id":   "{{file_id}}",
  "item_type": "file"
}

HTTP 201
[Captures]
final_share_id: jsonpath "$.id"

DELETE {{base_url}}/api/shares/{{final_share_id}}
Authorization: Bearer {{owner_token}}

HTTP 204


# ─────────────────────────────────────────────────────────────
# Step 9 — Cleanup. Admin deletes the test user; cascade reaps
#          the default Personal drive, root folder, and file.
# ─────────────────────────────────────────────────────────────
DELETE {{base_url}}/api/admin/users/{{owner_user_id}}
Authorization: Bearer {{admin_token}}

HTTP 200
