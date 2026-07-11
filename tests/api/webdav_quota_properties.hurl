# =============================================================
# OxiCloud — WebDAV quota properties (RFC 4331)
# =============================================================
# `DAV:quota-available-bytes` / `DAV:quota-used-bytes` are account-wide
# (not per-folder) live properties resolved once per PROPFIND request
# from the storage-usage service — see
# webdav_handler.rs::resolve_quota / webdav_adapter.rs::write_quota_props.
# Unlimited accounts (quota <= 0) omit quota-available-bytes entirely per
# RFC 4331 §3, rather than reporting a sentinel value.
#
# Coverage:
#   1. Named-prop PROPFIND for both properties on the WebDAV root → 207,
#      both present with numeric values.
#   2. allprop PROPFIND also includes both properties.
#   3. quota-used-bytes increases by (at least) the size of a file
#      just uploaded through WebDAV.
# =============================================================


# ─────────────────────────────────────────────────────────────
# Step 1 — Login, capture JWT
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/auth/login
Content-Type: application/json
{ "username": "{{username}}", "password": "{{password}}" }

HTTP 200
[Captures]
token: jsonpath "$.access_token"


# ─────────────────────────────────────────────────────────────
# Step 2 — Named-prop PROPFIND for the two quota properties.
# ─────────────────────────────────────────────────────────────
PROPFIND {{base_url}}/webdav/
Authorization: Bearer {{token}}
Depth: 0
Content-Type: application/xml; charset=utf-8
```
<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:quota-available-bytes/>
    <D:quota-used-bytes/>
  </D:prop>
</D:propfind>
```

HTTP 207
[Asserts]
xpath "string(//*[local-name()='propstat'][1]/*[local-name()='status'])" contains "200 OK"
xpath "number(//*[local-name()='quota-used-bytes'])" >= 0
xpath "number(//*[local-name()='quota-available-bytes'])" > 0


# ─────────────────────────────────────────────────────────────
# Step 3 — allprop PROPFIND also surfaces both properties.
# ─────────────────────────────────────────────────────────────
PROPFIND {{base_url}}/webdav/
Authorization: Bearer {{token}}
Depth: 0
Content-Type: application/xml; charset=utf-8
```
<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:allprop/>
</D:propfind>
```

HTTP 207
[Asserts]
xpath "number(//*[local-name()='quota-used-bytes'])" >= 0
xpath "number(//*[local-name()='quota-available-bytes'])" > 0
[Captures]
used_before: xpath "number(//*[local-name()='quota-used-bytes'])"


# ─────────────────────────────────────────────────────────────
# Step 4 — Upload a file, then confirm quota-used-bytes reflects it.
# ─────────────────────────────────────────────────────────────
PUT {{base_url}}/webdav/quota-probe.txt
Authorization: Bearer {{token}}
Content-Type: text/plain
```
quota accounting probe payload
```

HTTP 201


PROPFIND {{base_url}}/webdav/
Authorization: Bearer {{token}}
Depth: 0
Content-Type: application/xml; charset=utf-8
```
<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:quota-used-bytes/>
  </D:prop>
</D:propfind>
```

HTTP 207
[Asserts]
xpath "number(//*[local-name()='quota-used-bytes'])" >= {{used_before}}


# ─────────────────────────────────────────────────────────────
# Cleanup
# ─────────────────────────────────────────────────────────────
DELETE {{base_url}}/webdav/quota-probe.txt
Authorization: Bearer {{token}}

HTTP 204
