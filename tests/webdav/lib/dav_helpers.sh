#!/usr/bin/env bash
# Shared WebDAV / NC test helpers.
#
# Source order from a test_*.sh:
#
#   source test.env
#   source common.sh
#   source lib/dav_helpers.sh
#
# Depends on the following globals being already set:
#   $base_url, $username, $email, $password   (from test.env)
#   $TOKEN                                    (from `oxicloud_login`)
#
# Sets / exports:
#   $APP_PASS                                 (from `mint_app_password`)
#
# Functions provided:
#   mint_app_password   — mints an NC-compatible app password
#                         using the JWT and stores it in $APP_PASS
#   nc_curl …           — `curl` wrapper with Basic Auth pinned to
#                         the admin app password
#   api_curl …          — `curl` wrapper with the JWT bearer
#   count_responses BODY — count `<d:response>` children in a
#                         multistatus body
#   extract_href_for BODY SUBPATH
#                       — extract the first `<d:href>` value whose
#                         path ends with SUBPATH
#   assert_collection_hrefs_have_trailing_slash BODY
#                       — for every `<d:response>` in BODY that
#                         contains `<d:collection/>`, asserts that
#                         the `<d:href>` ends with `/`; otherwise
#                         asserts it does NOT end with `/`. This is
#                         the guard against the past regression where
#                         NC desktop aborted PROPFIND parsing because
#                         a folder href was emitted without trailing
#                         slash (RFC 4918 §5.2).
#   api_create_folder LABEL PARENT_ID
#                       — POST /api/folders, captures id into $LAST_FOLDER_ID
#   api_upload_file PATH FOLDER_ID
#                       — POST /api/files/upload, captures id into
#                         $LAST_FILE_ID and the content_hash into
#                         $LAST_FILE_CONTENT_HASH
#   api_delete_folder ID
#                       — DELETE /api/folders/{id} (soft-delete to trash)
#   api_empty_trash     — DELETE /api/trash/empty

# Global counters and pass/fail helpers that each test_*.sh may opt
# into. Tests that set their own PASS/FAIL can ignore these.
PASS=${PASS:-0}
FAIL=${FAIL:-0}
pass() { PASS=$(( PASS + 1 )); echo "  PASS: $*"; }
fail() { FAIL=$(( FAIL + 1 )); echo "  FAIL: $*" >&2; exit 1; }

mint_app_password() {
    local response
    response=$(curl -s -X POST \
        -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"label\":\"$(basename "${BASH_SOURCE[1]:-test}")\"}" \
        "$base_url/api/auth/app-passwords")
    APP_PASS=$(jq -r '.password // empty' <<< "$response" 2>/dev/null || echo "")
    [[ -n "$APP_PASS" ]] || fail "Could not mint app password (response: $response)"
}

# All NC-surface curls go through this helper so the Basic-Auth
# header (app password, not the user's login password) is applied
# uniformly. The NC handler rejects login-password Basic Auth.
nc_curl() {
    curl -s -u "$username:$APP_PASS" "$@"
}

# REST API curls use the JWT bearer.
api_curl() {
    curl -s -H "Authorization: Bearer $TOKEN" "$@"
}

# Native `/webdav/...` surface uses JWT bearer (same as REST), in
# contrast to the NC `/remote.php/dav/files/...` surface which uses
# Basic Auth with an app password (`nc_curl`). Same auth pattern as
# `tests/webdav/test_dedup_webdav_multichunk.sh`'s inline
# webdav_put/webdav_delete helpers, lifted here so Batch-5+
# native-DAV tests can stay DRY.
dav_curl() {
    curl -s -H "Authorization: Bearer $TOKEN" "$@"
}

# Return the HTTP status code of a `PROPFIND Depth: 0` against the
# given NC URL. Used by existence assertions ("did this collection
# silently get auto-created?") where the only thing the caller cares
# about is the status (404 → absent, 207 → present).
nc_status_propfind_depth0() {
    nc_curl -o /dev/null -w "%{http_code}" -X PROPFIND -H "Depth: 0" "$1"
}

# Count `<d:response>` (or `<D:response>`) children in a multistatus
# body. Case-insensitive on the namespace prefix because OxiCloud's
# two DAV surfaces use different cases: the NC handler emits
# `<d:response>` (lowercase prefix), while the native `/webdav/`
# handler emits `<D:response>` (uppercase prefix). RFC 4918 §14 only
# requires the URI to be `"DAV:"` — the prefix label is the
# implementer's choice.
#
# Always exits 0 — `grep -o` returns 1 when nothing matches, which
# under `set -euo pipefail` would abort the caller before the count
# is even examined. `|| true` here lets the helper return "0" for
# an empty / non-multistatus body so the calling test can assert
# explicitly on it.
count_responses() {
    { grep -oiE '<[dD]:response>' <<< "$1" || true; } | wc -l | tr -d ' '
}

# Extract the first `<d:href>` whose path contains the given suffix.
# Returns the raw href as-emitted by the server (no URL decoding);
# the suffix match is done on the raw form.
#
# Always exits 0 — empty stdout means "no href matched". Callers
# under `set -euo pipefail` rely on this: `VAR=$(extract_href_for …)`
# would otherwise abort the whole script when grep finds nothing,
# which is wrong for tests that legitimately want to assert
# "this href is absent" (D7 pinning Depth:infinity = Depth:1).
extract_href_for() {
    local body="$1" suffix="$2"
    # `grep -F … || true` swallows the no-match exit code (1) while
    # preserving real errors (2 → propagates because the outer
    # pipeline still has pipefail visibility into earlier stages).
    # Case-insensitive on the prefix — see `count_responses`.
    grep -oiE '<[dD]:href>[^<]+</[dD]:href>' <<< "$body" \
        | sed -E 's|^<[dD]:href>([^<]+)</[dD]:href>$|\1|' \
        | { grep -F "$suffix" || true; } \
        | head -n 1
}

# Extract the `<d:href>` from a `<d:response>` block whose body
# contains the given substring anywhere (e.g. an
# `<nc:trashbin-filename>` for trashbin responses, where the href
# itself only carries the opaque trash UUID — the filename only
# appears in the nc-namespace elements).
#
# Returns the raw href, or empty when no block matches. Same
# `pipefail`-safe semantics as `extract_href_for`.
extract_response_href_containing() {
    local body="$1" needle="$2"
    # Case-insensitive on the prefix — see `count_responses`.
    awk -v needle="$needle" '
        BEGIN { RS="</[dD]:response>" }
        index($0, needle) > 0 && match($0, /<[dD]:href>[^<]+<\/[dD]:href>/) {
            m = substr($0, RSTART, RLENGTH)
            sub(/^<[dD]:href>/,    "", m)
            sub(/<\/[dD]:href>$/,  "", m)
            print m
            exit
        }
    ' <<< "$body"
}

# Validate trailing-slash semantics across every <d:response> in a
# multistatus body. The function walks the body once and, per
# response, asserts:
#
#   - if <d:resourcetype> contains <d:collection/>, the matching
#     <d:href> MUST end with '/'
#   - otherwise the <d:href> MUST NOT end with '/'
#
# Exits via `fail` on the first violation. This is the regression
# guard for the NC desktop "Invalid href" parse error.
assert_collection_hrefs_have_trailing_slash() {
    local body="$1" label="${2:-multistatus}"
    # Use awk's RS to chunk the body by </d:response> or </D:response>
    # (case-insensitive on the prefix — see `count_responses` for why
    # both casings matter). For each chunk, find the href and check
    # for the collection marker.
    local pairs
    pairs=$(awk '
        BEGIN { RS = "</[dD]:response>" }
        /<[dD]:response>/ {
            href = ""
            is_coll = 0
            if (match($0, /<[dD]:href>[^<]+<\/[dD]:href>/)) {
                m = substr($0, RSTART, RLENGTH)
                # Strip the surrounding <d:href>/</d:href> tags
                # (either case).
                sub(/^<[dD]:href>/,    "", m)
                sub(/<\/[dD]:href>$/,  "", m)
                href = m
            }
            if ($0 ~ /<[dD]:collection\/>/) is_coll = 1
            if (href != "") print is_coll "|" href
        }
    ' <<< "$body")

    local line is_coll href violation=0
    while IFS='|' read -r is_coll href; do
        [[ -z "$href" ]] && continue
        if [[ "$is_coll" == "1" ]]; then
            if [[ "$href" != */ ]]; then
                echo "  FAIL[$label]: collection href without trailing slash: '$href'" >&2
                violation=1
            fi
        else
            if [[ "$href" == */ ]]; then
                echo "  FAIL[$label]: non-collection href with trailing slash: '$href'" >&2
                violation=1
            fi
        fi
    done <<< "$pairs"
    if [[ "$violation" -ne 0 ]]; then
        fail "[$label] trailing-slash semantics violated (see above)"
    fi
}

# ── REST API setup helpers ────────────────────────────────────────────────────

# POST /api/folders. Reads:
#   $1 — folder name
#   $2 — parent folder id (optional; passed verbatim as parent_id)
# Sets:
#   $LAST_FOLDER_ID — id of the new folder
api_create_folder() {
    local name="$1" parent_id="${2:-}"
    local body
    if [[ -n "$parent_id" ]]; then
        body=$(jq -n --arg n "$name" --arg p "$parent_id" \
            '{name:$n, parent_id:$p}')
    else
        body=$(jq -n --arg n "$name" '{name:$n}')
    fi
    local response
    # `/api/folders/` is the SINGLE-folder create (CreateFolderDto).
    # `/api/folders/create` is the BATCH endpoint — different DTO,
    # accepts an array; using it here returns 422 with an empty body
    # (which is what the original version of this helper was hitting
    # and failing on).
    response=$(api_curl -X POST \
        -H "Content-Type: application/json" \
        -d "$body" \
        "$base_url/api/folders")
    LAST_FOLDER_ID=$(jq -r '.id // empty' <<< "$response")
    [[ -n "$LAST_FOLDER_ID" ]] || fail "api_create_folder '$name': no id (response: $response)"
}

# POST /api/files/upload (multipart). Reads:
#   $1 — local fixture path
#   $2 — folder id (the file will be uploaded into this folder)
# Sets:
#   $LAST_FILE_ID            — id of the new file
#   $LAST_FILE_CONTENT_HASH  — content_hash from the response
api_upload_file() {
    local fixture="$1" folder_id="$2"
    local response
    response=$(api_curl -X POST \
        -F "folder_id=$folder_id" \
        -F "file=@$fixture" \
        "$base_url/api/files/upload")
    LAST_FILE_ID=$(jq -r '.id // empty' <<< "$response")
    LAST_FILE_CONTENT_HASH=$(jq -r '.content_hash // empty' <<< "$response")
    [[ -n "$LAST_FILE_ID" ]] || fail "api_upload_file '$fixture': no id (response: $response)"
}

api_delete_folder() {
    local id="$1"
    api_curl -X DELETE "$base_url/api/folders/$id" > /dev/null
}

api_delete_file() {
    local id="$1"
    api_curl -X DELETE "$base_url/api/files/$id" > /dev/null
}

api_empty_trash() {
    api_curl -X DELETE "$base_url/api/trash/empty" > /dev/null
}

# Wipe every child of the user's home folder + empty the trash, via
# the REST API. The home folder itself is preserved (it's a root
# folder, untouchable anyway).
#
# Useful as a defensive `wipe_home_folder` call at the START of any
# test that depends on a clean home state, so cross-test
# contamination from earlier scripts (e.g. orphans left by handlers
# that 500-leak on conflict, or pinned-bug scenarios that
# deliberately leave half-cleaned state) never poisons later
# assertions. Requires `$HOME_FOLDER_ID` to be set first via
# `resolve_home_folder_id`.
wipe_home_folder() {
    [[ -n "${HOME_FOLDER_ID:-}" ]] \
        || fail "wipe_home_folder: HOME_FOLDER_ID is unset — call resolve_home_folder_id first"
    # `/listing` (NOT `/contents`) is the endpoint that returns the
    # `.files[]` / `.folders[]` arrays we iterate here — same one
    # `tests/api/storage_cleanup_check.sh` uses for the equivalent
    # full-tree wipe before its disk-audit step.
    local listing
    listing=$(api_curl "$base_url/api/folders/$HOME_FOLDER_ID/listing")
    # Delete every direct child file (recursive contents go with the
    # file's row). Errors are swallowed because the test that called
    # us doesn't care WHY a leftover was unreachable — it just wants
    # the slate clean.
    while IFS= read -r fid; do
        [[ -z "$fid" || "$fid" == "null" ]] && continue
        api_curl -X DELETE "$base_url/api/files/$fid" > /dev/null 2>&1 || true
    done < <(jq -r '.files[]?.id   // empty' <<< "$listing")
    # Then every direct child folder (recursive subtree goes with).
    while IFS= read -r fid; do
        [[ -z "$fid" || "$fid" == "null" ]] && continue
        api_curl -X DELETE "$base_url/api/folders/$fid" > /dev/null 2>&1 || true
    done < <(jq -r '.folders[]?.id // empty' <<< "$listing")
    # Finally permanently delete everything in trash so the row-level
    # `is_trashed` orphans the upstream tests left behind don't make
    # *us* leak chunks/blobs into storage_cleanup_check's audit.
    api_empty_trash
}

# Resolve the user's home folder id (parent_id IS NULL, first entry).
# Sets:
#   $HOME_FOLDER_ID
resolve_home_folder_id() {
    local response
    response=$(api_curl "$base_url/api/folders")
    HOME_FOLDER_ID=$(jq -r '.[0].id // empty' <<< "$response")
    [[ -n "$HOME_FOLDER_ID" ]] || fail "Could not resolve home folder id (response: $response)"
}
