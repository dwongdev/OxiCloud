# OxiCloud ReBAC — Permissions, Grants, and Cascading

## Context

OxiCloud currently has a binary authorization model: the owner of a folder/file has every permission, every non-owner has none. The only user-to-user sharing is via anonymous token links (`storage.shares`) with three coarse flags (read/write/reshare). There is no way for a user to grant a named user fine-grained access to a folder, and no way to list resources others have shared with them.

This plan introduces a Relationship-Based Access Control (ReBAC) model:

- 6 named permissions: `read`, `create`, `share`, `comment`, `delete`, `update`
- Cascading: a grant on a folder applies to all descendants (sub-folders + files) via the existing `storage.folders.lpath` ltree
- Subjects: `user` (v1), `group` (future placeholder in schema), `token` (anonymous links — unified with existing `storage.shares`), `external` (future in schema for federated identities: Open Cloud Mesh / external OIDC)
- Pluggable engine: a single `AuthorizationEngine` trait, default implementation in PostgreSQL, ready for an `OpenFgaEngine` later
- **Roles** (Viewer, Commenter, Editor, Manager, Admin) as a UX/DTO sugar layer that the server expands into the underlying permission rows — storage and engine know nothing about roles

User decisions confirmed in conversation:
1. **Implicit owner** — owners have no rows in `access_grants`; the engine short-circuits when the caller is the resource's owner.
2. **`share` permission lets the holder grant to other named users** via `POST /api/grants` (not just create anonymous links).
3. **`GET /api/grants/incoming` returns direct grants only** — one row per resource explicitly granted to the caller. UI drills in via existing listing endpoints.
4. **Unify anonymous link shares under `access_grants`** with `subject_type='token'`. `storage.shares` retains token-lifecycle metadata (password, expiry, access count) only; the permission flags move to `access_grants`. One-time data migration.
5. **Roles in v1, implication chains deferred** — roles bundle the 6 raw permissions at the DTO layer (no schema impact). The storage keeps one row per granted permission. Permission implication (e.g., `update` ⊃ `comment` ⊃ `read`) is a Future optimization that compresses storage but doesn't change observable behavior.
6. **6 permissions are final for v1** — `read`, `create`, `share`, `comment`, `delete`, `update`. `download` (preview-only vs full-bytes) is a candidate for v2 if a "view-only" feature is added; trivial ALTER on the CHECK constraint then.
7. **Schema reserves `subject_type='external'` for federated identities** (Open Cloud Mesh / external OIDC). v1 adds the enum value and the `Subject::External(Uuid)` variant; the lookup table `auth.external_subjects` and the federation middleware are deferred.
8. **Architectural rule: AuthZ lives in the service layer, never in handlers.** All permission checks go through `AuthorizationEngine` via service methods. HTTP handlers (REST, WebDAV, NextCloud, CalDAV, CardDAV) authenticate the caller and pass `caller_id` into the service — they do NOT perform their own ownership/permission checks. This rule must be documented in `CLAUDE.md`.
9. **Per-row storage, not bitmap.** One row per `(subject, resource, permission)` rather than a single row with a packed bitmap. Preserves per-permission `granted_at` and `granted_by` (audit value), keeps future per-grant `expires_at` an easy addition, and maps 1:1 to OpenFGA tuples. Storage cost at OxiCloud's scale is acceptable and not on a hot path — micro-optimization deferred indefinitely. Matches the per-tuple shape used by Zanzibar, SpiceDB, OpenFGA, Permify.

Out-of-scope (deferred):
- Group creation & membership UI (the schema reserves `subject_type='group'`, but no group CRUD endpoints in this plan)
- External-user federation (`subject_type='external'` reserved in schema; `auth.external_subjects` table + OCM/OIDC federation middleware come later)
- Permission implication graph (`update` ⊃ `comment` ⊃ `read`, etc.) — storage compression with no observable behavior change
- Negative grants / deny rules (model stays additive — union of all applicable grants)
- Grant expiry per-row (token expiry stays on `storage.shares`)
- Comment feature itself (the `comment` permission is reserved; the comments table is a future feature)
- `download` permission (separation of preview-only from full-bytes export)
- Decision caching (in-process + Redis L2) — see "Future: caching layer" below

---

## Architecture overview

```
┌────────────────────┐
│   HTTP handlers    │  POST/GET/DELETE /api/grants
└──────────┬─────────┘
           ▼
┌────────────────────┐
│  FolderService     │ ────► authz.require(caller, Update, Folder(id))
│  FileManagementSvc │ ────► authz.require(caller, Create, Folder(parent))
│  FileRetrievalSvc  │ ────► authz.require(caller, Read,   Folder(id))
│  ShareService      │ ────► token grants written via authz.grant(Token(t), ...)
└──────────┬─────────┘
           ▼  Arc<dyn AuthorizationEngine>
┌─────────────────────────────────────────────┐
│  AuthorizationEngine trait                  │
│   • check(subject, perm, resource) → bool   │
│   • require(...)                            │
│   • grant / revoke                          │
│   • list_incoming / list_on_resource        │
└──────────┬────────────────────────┬─────────┘
           ▼                        ▼
  PgAclEngine (v1, default)   OpenFgaEngine (future)
       ▼
  storage.access_grants  +  storage.folders.lpath (cascading)
```

---

## Schema

### New table: `storage.access_grants`

```sql
CREATE TABLE storage.access_grants (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Subject (who has the permission)
    -- 'user'     — auth.users.id
    -- 'group'    — future: group membership
    -- 'token'    — refers to storage.shares.id (anonymous link)
    -- 'external' — future: refers to auth.external_subjects.id (OCM / federated OIDC)
    subject_type    TEXT NOT NULL CHECK (subject_type IN ('user', 'group', 'token', 'external')),
    subject_id      UUID NOT NULL,

    -- Resource (what the permission is on)
    resource_type   TEXT NOT NULL CHECK (resource_type IN ('folder', 'file')),
    resource_id     UUID NOT NULL,

    -- Permission (what action is allowed)
    permission      TEXT NOT NULL CHECK (permission IN
                      ('read', 'create', 'share', 'comment', 'delete', 'update')),

    -- Audit
    granted_by      UUID NOT NULL,                              -- user_id who created the grant
    granted_at      TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (subject_type, subject_id, resource_type, resource_id, permission)
);

CREATE INDEX idx_grants_subject  ON storage.access_grants (subject_type, subject_id);
CREATE INDEX idx_grants_resource ON storage.access_grants (resource_type, resource_id);
```

`granted_by` is always a user (group cannot grant). No FK to `auth.users` on `subject_id` or `granted_by` — those tables are in a different schema and the values are polymorphic.

### Cleanup of `storage.shares`

The permission columns move to `access_grants`. `storage.shares` keeps token-lifecycle metadata.

```sql
-- After data migration (below):
ALTER TABLE storage.shares
    DROP COLUMN permissions_read,
    DROP COLUMN permissions_write,
    DROP COLUMN permissions_reshare;
```

### Data migration (one-off, in the same migration file)

Each existing share becomes one or more rows in `access_grants` with `subject_type='token'`, `subject_id=shares.id`:

```sql
INSERT INTO storage.access_grants
    (subject_type, subject_id, resource_type, resource_id, permission, granted_by)
SELECT 'token', s.id, s.item_type, s.item_id::uuid, 'read', s.created_by
  FROM storage.shares s
 WHERE s.permissions_read;

-- 'write' on the old model implies full mutation rights for the link holder.
-- Mapped to read + create + update + delete in the new model.
INSERT INTO storage.access_grants (subject_type, subject_id, resource_type, resource_id, permission, granted_by)
SELECT 'token', s.id, s.item_type, s.item_id::uuid, p.perm, s.created_by
  FROM storage.shares s
  CROSS JOIN (VALUES ('create'), ('update'), ('delete')) AS p(perm)
 WHERE s.permissions_write;

INSERT INTO storage.access_grants (subject_type, subject_id, resource_type, resource_id, permission, granted_by)
SELECT 'token', s.id, s.item_type, s.item_id::uuid, 'share', s.created_by
  FROM storage.shares s
 WHERE s.permissions_reshare;
```

---

## Lifecycle and grant cleanup (v1 — correctness requirement)

When a resource or subject is **permanently** deleted, all `access_grants` rows referring to it must be removed. Otherwise:
- Orphan grants linger forever
- A future UUID reuse (unlikely but possible) could match a stale row
- "Shared with me" returns grants on resources that no longer exist
- Audit queries (`COUNT(*) FROM access_grants`) drift away from reality

### What triggers cleanup, and what doesn't

| Event | Affected grants | Action |
|---|---|---|
| Folder **permanently** deleted | `resource_type='folder', resource_id=F` (plus all descendant files via FK cascade chain) | DELETE |
| File **permanently** deleted | `resource_type='file', resource_id=X` | DELETE |
| Folder/file moved to **trash** (soft) | None | **No-op** — restore must resume access |
| Folder/file **restored** from trash | None | No-op |
| Trash **emptied** (permanent destruction) | Same as permanent delete | DELETE |
| User deleted | `subject_type='user', subject_id=U`. `granted_by=U` is left as-is (audit trail). | DELETE the subject rows; keep granter UUIDs |
| Anonymous share token deleted | `subject_type='token', subject_id=T` | DELETE |
| Group deleted (future) | `subject_type='group', subject_id=G` | DELETE |

### Defense-in-depth: DB triggers in the same migration

Even if a future code path bypasses the service layer (admin scripts, bulk maintenance, manual SQL), the database enforces cleanup:

```sql
CREATE OR REPLACE FUNCTION storage.cleanup_grants_on_resource_delete()
RETURNS TRIGGER AS $$
BEGIN
    DELETE FROM storage.access_grants
     WHERE resource_type = TG_ARGV[0]
       AND resource_id   = OLD.id;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_cleanup_grants_folder
    AFTER DELETE ON storage.folders
    FOR EACH ROW
    EXECUTE FUNCTION storage.cleanup_grants_on_resource_delete('folder');

CREATE TRIGGER trg_cleanup_grants_file
    AFTER DELETE ON storage.files
    FOR EACH ROW
    EXECUTE FUNCTION storage.cleanup_grants_on_resource_delete('file');

CREATE OR REPLACE FUNCTION storage.cleanup_grants_on_subject_delete()
RETURNS TRIGGER AS $$
BEGIN
    DELETE FROM storage.access_grants
     WHERE subject_type = TG_ARGV[0]
       AND subject_id   = OLD.id;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_cleanup_grants_user
    AFTER DELETE ON auth.users
    FOR EACH ROW
    EXECUTE FUNCTION storage.cleanup_grants_on_subject_delete('user');

CREATE TRIGGER trg_cleanup_grants_token
    AFTER DELETE ON storage.shares
    FOR EACH ROW
    EXECUTE FUNCTION storage.cleanup_grants_on_subject_delete('token');
```

`storage.files.folder_id REFERENCES storage.folders(id) ON DELETE CASCADE` already exists — when a folder is permanently deleted, the file trigger fires for each cascaded child. No need to walk the ltree subtree manually.

### Application-layer cleanup (explicit hooks)

The trait gains two cleanup methods so the application layer can invoke cleanup explicitly. This matters because a future cache layer (see Future section) needs to see the invalidation event at the engine boundary — DB triggers happen below the cache:

```rust
/// Removes all grants targeting this resource. Returns count removed.
async fn revoke_all_for_resource(&self, resource: Resource)
    -> Result<usize, DomainError>;

/// Removes all grants where this subject is the holder.
async fn revoke_all_for_subject(&self, subject: Subject)
    -> Result<usize, DomainError>;
```

Service call sites:

| Service method | Cleanup call |
|---|---|
| `FolderService::delete_folder_with_perms` (permanent delete) | `authz.revoke_all_for_resource(Folder(id))` |
| `FileManagementService::delete_file_with_perms` | `authz.revoke_all_for_resource(File(id))` |
| `FileManagementService::delete_and_cleanup_with_perms` | Same |
| `TrashService::delete_permanently` | `authz.revoke_all_for_resource(...)` per item |
| `TrashService::empty_trash` | Loop over items, same call |
| `TrashService::move_to_trash` | **No cleanup** (soft delete; grants preserved for eventual restore) |
| `TrashService::restore_item` | No action (grants are still there) |
| `ShareService::delete_shared_link` | `authz.revoke_all_for_subject(Token(share_id))` |
| `AuthApplicationService::delete_user` (admin) | `authz.revoke_all_for_subject(User(user_id))` |

DB triggers stay as defense-in-depth — they catch anything the application forgets, and they catch bulk maintenance operations. The application-layer hook is the canonical path; the trigger is the safety net.

### File lifecycle hook integration

There's an existing `FileDeletedHook` trait in `src/application/ports/file_lifecycle.rs` that already fires after permanent file deletion (used today for blob-ref-count decrement). Implement an additional hook:

```rust
struct GrantCleanupHook { authz: Arc<dyn AuthorizationEngine> }

#[async_trait::async_trait]
impl FileDeletedHook for GrantCleanupHook {
    async fn on_file_deleted(&self, file_id: Uuid) -> Result<(), DomainError> {
        self.authz.revoke_all_for_resource(Resource::File(file_id)).await?;
        Ok(())
    }
}
```

Register it in `common/di.rs` alongside the existing hooks. The folder/user/token cases get inline calls in their respective services (no hook trait for those yet — adding one if it's needed for a third caller is a future refactor).

### Verification — lifecycle scenarios in `grants.hurl`

1. **Resource delete clears grants**
   - Alice creates folder F, grants Bob read, then permanently deletes F (via empty trash)
   - Bob's `GET /api/grants/incoming` returns 0 entries containing F
   - Direct SQL check (in a debug endpoint or via a test fixture): `SELECT COUNT(*) FROM access_grants WHERE resource_id = F` is 0

2. **Trash retains grants**
   - Alice grants Bob read on F, moves F to trash, then restores F
   - Bob still has `read` access after restore (regression: before any lifecycle change, this must continue to work)

3. **User delete clears subject grants but preserves granter**
   - Alice grants Bob and Carol read on F. Admin deletes Bob.
   - Carol's grant on F survives; her `granted_by=alice` still references Alice (intact)
   - Bob's row is gone

4. **Token delete clears token grants**
   - Alice creates a public share link on F → `access_grants` has rows with `subject_type='token'`
   - Alice deletes the share link → token rows in `access_grants` are gone

5. **Orphan invariant (post-test SQL)**
   ```sql
   SELECT COUNT(*) FROM storage.access_grants g
    WHERE (g.resource_type = 'folder'
           AND NOT EXISTS (SELECT 1 FROM storage.folders WHERE id = g.resource_id))
       OR (g.resource_type = 'file'
           AND NOT EXISTS (SELECT 1 FROM storage.files WHERE id = g.resource_id));
   ```
   Must always be 0 after every Hurl run.

---

## Domain types

New module `src/domain/services/authorization.rs`:

```rust
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Subject {
    User(Uuid),
    Group(Uuid),     // schema reserved, no CRUD endpoints in v1
    Token(Uuid),     // refers to storage.shares.id
    External(Uuid),  // future: refers to auth.external_subjects.id (OCM / federated OIDC)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Resource {
    Folder(Uuid),
    File(Uuid),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Permission {
    Read, Create, Share, Comment, Delete, Update,
}

pub struct Grant {
    pub id: Uuid,
    pub subject: Subject,
    pub resource: Resource,
    pub permission: Permission,
    pub granted_by: Uuid,
    pub granted_at: chrono::DateTime<chrono::Utc>,
}
```

Conversion helpers (`as_str()` for SQL binding, `TryFrom<&str>` for row decoding) live alongside.

---

## Port: `AuthorizationEngine`

New file `src/application/ports/authorization_ports.rs`:

```rust
use crate::common::errors::DomainError;
use crate::domain::services::authorization::{Grant, Permission, Resource, Subject};

#[async_trait::async_trait]
pub trait AuthorizationEngine: Send + Sync + 'static {
    /// Returns true if `subject` has `permission` on `resource`,
    /// considering owner short-circuit AND cascading from folder ancestors.
    async fn check(
        &self,
        subject: Subject,
        permission: Permission,
        resource: Resource,
    ) -> Result<bool, DomainError>;

    /// Convenience: returns Ok(()) when check passes; DomainError::not_found
    /// otherwise (anti-enumeration — same error for "no such resource" and
    /// "exists but you can't see it").
    async fn require(
        &self,
        subject: Subject,
        permission: Permission,
        resource: Resource,
    ) -> Result<(), DomainError> {
        if self.check(subject, permission, resource).await? {
            Ok(())
        } else {
            let (kind, id) = match resource {
                Resource::Folder(id) => ("Folder", id),
                Resource::File(id)   => ("File", id),
            };
            Err(DomainError::not_found(kind, id.to_string()))
        }
    }

    /// Resources explicitly granted to `subject`. Direct grants only — no
    /// cascade expansion. Used by GET /api/grants/incoming.
    async fn list_incoming_grants(
        &self,
        subject: Subject,
        permission_filter: Option<Permission>,
    ) -> Result<Vec<Grant>, DomainError>;

    /// All grants on a specific resource (for "Manage sharing" UI).
    /// Caller-side must verify the caller has `share` on the resource.
    async fn list_grants_on_resource(
        &self,
        resource: Resource,
    ) -> Result<Vec<Grant>, DomainError>;

    /// Idempotent (UNIQUE constraint absorbs duplicates).
    async fn grant(
        &self,
        granted_by: Uuid,
        subject: Subject,
        permission: Permission,
        resource: Resource,
    ) -> Result<Grant, DomainError>;

    /// Revoke by id.
    async fn revoke(&self, grant_id: Uuid) -> Result<(), DomainError>;
}
```

Wired into `AppState` in `src/common/di.rs` as `pub authorization: Arc<dyn AuthorizationEngine>`. The factory selects the implementation from `OXICLOUD_AUTHZ_ENGINE` env var (default: `postgres`).

---

## PgAclEngine implementation

New file `src/infrastructure/services/pg_acl_engine.rs`. Holds `Arc<DbPools>`, `Arc<FolderDbRepository>`, `Arc<FileBlobReadRepository>` (for owner lookups).

### `check()` algorithm

```rust
async fn check(&self, subject: Subject, perm: Permission, resource: Resource) -> Result<bool, _> {
    // Step 1 — owner short-circuit (only for user subjects)
    if let Subject::User(uid) = subject {
        let owner = match resource {
            Resource::Folder(id) => self.folder_repo.get_folder_user_id(&id.to_string()).await?,
            Resource::File(id)   => self.file_repo.get_file_user_id(&id.to_string()).await?,
        };
        if owner == uid { return Ok(true); }
    }

    // Step 2 — direct or cascading grant via SQL
    self.grant_exists(subject, perm, resource).await
}
```

### Cascading SQL — folders

```sql
SELECT EXISTS (
    SELECT 1
      FROM storage.access_grants g
      JOIN storage.folders gf ON gf.id = g.resource_id
     WHERE g.subject_type = $1 AND g.subject_id = $2
       AND g.permission   = $3
       AND g.resource_type = 'folder'
       AND gf.lpath @> (SELECT lpath FROM storage.folders WHERE id = $4)
)
```

`gf.lpath @> target.lpath` means "gf is an ancestor of (or equal to) target". Uses the existing GiST index `idx_folders_lpath` — O(log N).

### Cascading SQL — files

A file inherits from its containing folder. Two-branch query:

```sql
SELECT EXISTS (
    -- direct file grant
    SELECT 1 FROM storage.access_grants
     WHERE subject_type = $1 AND subject_id = $2 AND permission = $3
       AND resource_type = 'file' AND resource_id = $4
    UNION ALL
    -- cascading from any ancestor folder of the file's containing folder
    SELECT 1
      FROM storage.access_grants g
      JOIN storage.folders gf      ON gf.id = g.resource_id
      JOIN storage.files target_f  ON target_f.id = $4
     WHERE g.subject_type = $1 AND g.subject_id = $2
       AND g.permission = $3
       AND g.resource_type = 'folder'
       AND target_f.folder_id IS NOT NULL
       AND gf.lpath @> (SELECT lpath FROM storage.folders WHERE id = target_f.folder_id)
)
```

Files at root (`folder_id IS NULL`) only match the direct branch.

### Engine selection in `AppState`

```rust
// src/common/di.rs (build_app_state)
let authz: Arc<dyn AuthorizationEngine> = match env::var("OXICLOUD_AUTHZ_ENGINE").as_deref() {
    Ok("openfga") => unimplemented!("OpenFgaEngine — future"),
    _ => Arc::new(PgAclEngine::new(
        pools.clone(),
        repositories.folder_repository.clone(),
        repositories.file_read_repository.clone(),
    )),
};
```

---

## Service integration

Each `*_with_perms` method already calls `verify_owner`. Replace the call with `authz.require(...)`. The semantics broaden (grants count, not just ownership) but the signature and error mapping stay the same.

### Folder permission mapping (folder_service.rs)

| Method | Permission(s) checked |
|---|---|
| `create_folder_with_perms(dto, caller)` | `Create` on `Folder(parent_id)` |
| `get_folder_with_perms(id, caller)` | `Read` on `Folder(id)` |
| `rename_folder_with_perms(id, dto, caller)` | `Update` on `Folder(id)` |
| `move_folder_with_perms(id, dto, caller)` | `Update` on `Folder(id)` AND `Create` on `Folder(new_parent)` |
| `delete_folder_with_perms(id, caller)` | `Delete` on `Folder(id)` |

### File permission mapping (file_management_service.rs)

| Method | Permission(s) checked |
|---|---|
| `move_file_with_perms(file_id, caller, target)` | `Update` on `File(file_id)` AND `Create` on `Folder(target)` if target is Some |
| `copy_file_with_perms(file_id, caller, target)` | `Read` on `File(file_id)` AND `Create` on `Folder(target)` if target is Some |
| `rename_file_with_perms(file_id, caller, name)` | `Update` on `File(file_id)` |
| `delete_file_with_perms(id, caller)` | `Delete` on `File(id)` |
| `copy_folder_tree_with_perms(src, caller, target, name)` | `Read` on `Folder(src)` AND `Create` on `Folder(target)` if target is Some |

### File retrieval mapping (file_retrieval_service.rs)

`get_file_owned`, `list_files_owned`, `get_file_stream_owned`, `get_file_optimized_owned`, `get_file_range_stream_owned`, `list_files_batch_for_owner` → each becomes `authz.require(caller, Read, File(id))` before delegating to the unchecked variant.

### Path-based lookups (currently unchecked IDOR risk)

`folder_service::get_folder_by_path(path)` and `file_retrieval_service::get_file_by_path(path)` resolve a path then return the resource without any check. After this plan: resolve, then `authz.require(caller, Read, …)`. This closes a known IDOR documented in the previous plan's "Out of scope" section.

### Owner short-circuit ensures zero behavior change for current users

Because every existing user-vs-own-resource interaction is an owner check, the engine's owner short-circuit makes those calls equivalent to the current `verify_owner`. No grant lookups on the hot path until a real cross-user grant exists.

---

## REST endpoints

New handler `src/interfaces/api/handlers/grant_handler.rs`. Registered under `/api/grants`.

### `POST /api/grants` — create a grant

```json
{
  "subject":  { "type": "user",   "id": "<uuid>" },
  "resource": { "type": "folder", "id": "<uuid>" },
  "permissions": ["read", "comment"]
}
```

Behavior:
1. Authenticated caller required.
2. `authz.require(caller, Share, resource)` — caller must have `share` on the resource (owners always pass via short-circuit).
3. For each permission in the list: `authz.grant(caller_id, subject, perm, resource)`. UNIQUE constraint makes repeats no-ops.
4. Returns 201 with the list of created/existing grants.

### `DELETE /api/grants/{id}` — revoke a grant

1. Look up the grant.
2. Allow if caller is the grant's `granted_by` user OR caller has `share` on the underlying resource.
3. `authz.revoke(id)`.
4. Returns 204.

### `GET /api/grants/incoming?permission=read&type=folder` — what others have shared with me

Direct grants only (per user decision). Subject is the authenticated caller's `User(id)`. Optional filters by permission and resource type.

Returns:
```json
[
  {
    "id": "<grant-uuid>",
    "resource": { "type": "folder", "id": "<uuid>", "name": "Photos", "path": "..." },
    "permission": "read",
    "granted_by": { "id": "<uuid>", "username": "alice" },
    "granted_at": "2026-05-20T10:51:13Z"
  }
]
```

Resource name/path is enriched via a JOIN to `storage.folders` / `storage.files`.

### `GET /api/grants?resource_type=folder&resource_id={id}` — list grants on a resource

Requires `authz.require(caller, Share, resource)` (you can see who has access only if you can manage sharing).

Returns the same shape as incoming, but for the specified resource.

### `GET /api/grants/outgoing` — grants I have created

Filtered by `granted_by = caller_id`. Useful for "Manage all my shares" UI.

---

## Roles (UX / DTO layer)

Roles are **preset bundles of permissions** that the API exposes for UI convenience. The server expands a role into its underlying permission list before writing rows; storage and engine know nothing about roles.

### Role catalog

| Role | Permissions |
|---|---|
| `Viewer`    | `read` |
| `Commenter` | `read`, `comment` |
| `Editor`    | `read`, `comment`, `create`, `update` |
| `Manager`   | `read`, `comment`, `create`, `update`, `share` |
| `Admin`     | `read`, `comment`, `create`, `update`, `share`, `delete` |

Defined as a Rust enum in `src/application/dtos/grant_dto.rs`:

```rust
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role { Viewer, Commenter, Editor, Manager, Admin }

impl Role {
    pub fn expand(self) -> &'static [Permission] {
        match self {
            Role::Viewer    => &[Permission::Read],
            Role::Commenter   => &[Permission::Read, Permission::Comment],
            Role::Contributor => &[Permission::Read, Permission::Create],
            Role::Editor      => &[Permission::Read, Permission::Comment,
                                   Permission::Create, Permission::Update],
            Role::Owner       => &[Permission::Read, Permission::Comment,
                                   Permission::Create, Permission::Update,
                                   Permission::Share, Permission::Delete,
                                   Permission::Manage],
        }
    }
}
```

> **Note (D-Prep, 2026-06-17):** the `Manager` role was retired before shipping
> (its bundle was a strict subset of `Owner`); the historical `Admin` role was
> renamed to `Owner` to disambiguate from `UserRole::Admin` (the user-account
> privilege) and match Drive plan terminology. `Contributor` is the new
> drop-zone role. The actual on-the-wire enum lives in
> `src/application/dtos/grant_dto.rs`; that file is the canonical source of
> truth for bundle expansion. The pivot to role-keyed storage (`role_grants`
> table) also happened in D-Prep — see
> `docs/architecture/rebac-authorization.md` for the dual-write timeline.

### `POST /api/grants` accepts either shape

```json
// Either explicit permissions:
{ "subject":  { "type": "user", "id": "<uuid>" },
  "resource": { "type": "folder", "id": "<uuid>" },
  "permissions": ["read", "comment"] }

// Or a role:
{ "subject":  { "type": "user", "id": "<uuid>" },
  "resource": { "type": "folder", "id": "<uuid>" },
  "role": "editor" }
```

The DTO uses `#[serde(untagged)]` or two separate fields with server-side validation that exactly one is provided. Server expands `role` → permission list, then writes the rows.

### `PUT /api/grants/role` — reconcile a subject's role on a resource

```json
{ "subject":  { "type": "user", "id": "<uuid>" },
  "resource": { "type": "folder", "id": "<uuid>" },
  "role": "manager" }
```

Behavior:
1. `authz.require(caller, Share, resource)`.
2. Read the current set of permissions held by `subject` on `resource`.
3. Compute the diff vs `role.expand()`: which permissions to INSERT, which to DELETE.
4. Apply both in one transaction.
5. Returns 200 with the new full set.

This is the canonical way for a UI to set "Bob is now Editor of /Photos" — the frontend doesn't track which specific rows exist.

### Why roles are pure DTO sugar (not stored)

- **Roles can evolve without schema migrations** — adding "Reviewer" tomorrow is a code change, no ALTER.
- **Mixing is allowed** — a future UI can start from "Editor" and add `share` manually; the result is a custom mixture, not "Editor + share".
- **OpenFGA migration unaffected** — tuples are per-permission regardless of how they were granted.
- **Revocation is granular** — removing a single permission doesn't require touching a "role" abstraction.

---

## File changes

### New files
- `migrations/2026MMDDHHMMSS_rebac_access_grants.sql` — table + indexes + data migration from `storage.shares`
- `src/domain/services/authorization.rs` — `Subject` (including `External` variant), `Resource`, `Permission`, `Grant` enums/structs
- `src/application/ports/authorization_ports.rs` — `AuthorizationEngine` trait
- `src/infrastructure/services/pg_acl_engine.rs` — default impl
- `src/interfaces/api/handlers/grant_handler.rs` — REST endpoints (`POST/DELETE/GET /api/grants`, `PUT /api/grants/role`, `GET /api/grants/incoming|outgoing`)
- `src/application/dtos/grant_dto.rs` — request/response DTOs including `Role` enum + `Role::expand()`

### Modified
- `CLAUDE.md` — add a section under the Backend Architecture documenting the rule: **AuthZ is enforced exclusively in the application service layer. HTTP handlers (REST, WebDAV, NextCloud, CalDAV, CardDAV) only authenticate the caller and pass `caller_id` to the service. Never duplicate permission checks at the exposition layer.** This prevents drift between layers and matches the existing pattern of `*_with_perms` methods.
- `src/common/di.rs` — wire `authz` into `AppState`; inject into Folder/FileManagement/FileRetrieval services
- `src/application/services/folder_service.rs` — replace `verify_owner` calls with `authz.require`; add path-based check to `get_folder_by_path`
- `src/application/services/file_management_service.rs` — same; remove the private `verify_target_folder_owner` wrapper (engine does both)
- `src/application/services/file_retrieval_service.rs` — replace owner checks; add path-based check to `get_file_by_path`
- `src/application/services/share_service.rs` — on `create_shared_link`, also write the corresponding `access_grants` rows so that token-based access goes through the engine uniformly
- `src/interfaces/api/routes.rs` — register `/api/grants` routes
- `src/application/ports/mod.rs` — `pub mod authorization_ports`
- `src/domain/services/mod.rs` — `pub mod authorization`
- `src/infrastructure/services/mod.rs` — `pub mod pg_acl_engine`

### Removed
- The fields `permissions_read`, `permissions_write`, `permissions_reshare` from `storage.shares` (and their domain/dto representations) — replaced by `access_grants` rows. Migration script preserves existing data.

---

## Verification

### Build & lint
```
cargo fmt --all
cargo clippy --all-features --all-targets -- -D warnings
cargo test --workspace
```

### Hurl integration tests (new file `tests/api/grants.hurl`)

Run via the existing `tests/api/run.sh` (add `permissions.hurl` AND the new `grants.hurl` to the runner).

Setup (admin token + bob token, both already available from `permissions.hurl`):

1. **Grant + check**
   - Alice creates folder `/api/folders {parent: home, name: "Shared"}` → captures `folder_id`
   - Alice grants Bob `read` on the folder: `POST /api/grants` with subject=user/bob, resource=folder/Shared, perms=[read]
   - Bob calls `GET /api/folders/{folder_id}/contents` → 200 (was 404 before grant)
   - Bob calls `PUT /api/folders/{folder_id}/rename` → 404 (no `update` grant)

2. **Cascading**
   - Alice creates a sub-folder `Shared/Inner`
   - Alice uploads a file `vacation.jpg` inside `Inner`
   - Bob (with `read` on `Shared`) calls `GET /api/files/{file_id}` → 200 (cascaded via lpath)

3. **Incoming list**
   - Bob calls `GET /api/grants/incoming` → returns 1 entry with the folder, permission=read

4. **Re-share**
   - Carol (new user) — Alice grants Bob `share` additionally
   - Bob now successfully calls `POST /api/grants` to grant Carol `read`
   - Carol calls `GET /api/folders/{folder_id}/contents` → 200

5. **Revoke**
   - Alice deletes Bob's grant via `DELETE /api/grants/{grant_id}` → 204
   - Bob's `GET /api/folders/{folder_id}/contents` → 404

6. **Roles**
   - Alice grants `POST /api/grants` with `role: "editor"` for Bob on a new folder
   - Bob can read AND rename a file inside (Editor includes `update`)
   - Bob CANNOT delete the folder (Editor excludes `delete`) → 404
   - Alice calls `PUT /api/grants/role` with `role: "admin"` for Bob
   - Bob can now delete the folder → 200
   - Alice calls `PUT /api/grants/role` with `role: "viewer"` for Bob
   - Bob loses update/delete/comment/create/share; can only read → rename returns 404

7. **Token unification (regression)**
   - `permissions.hurl` already covers existing share-link flows. After migration, those still pass — the engine reads from `access_grants` for token subjects, transparently.

### Unit tests
- New tests in `src/application/services/idor_protection_test.rs`:
  - `engine.check(non_owner, Read, file)` with no grant → false
  - `engine.check(owner, _, _)` → true (owner short-circuit) without touching `access_grants`
  - `engine.check(grantee, Read, file)` after `grant()` → true
  - Cascade: grant on parent folder → child file check returns true
  - Revoke removes the row → next check returns false
- Tests use a stub repo for owners and an in-memory grant store, OR run against the real PG via the existing test harness.

### Storage growth sanity check (manual)
- Before migration: count rows in `storage.shares`.
- After migration: count rows in `storage.access_grants` with `subject_type='token'` ≈ shares × {1 + flag count}.
- Confirm no owner-self rows were created (validates implicit-owner choice).

---

## Rollout sequencing

1. **PR 1** — migration + schema (creates `access_grants`, migrates `storage.shares` permission flags). No code changes yet. Deploy and verify the migration runs cleanly.
2. **PR 2** — `AuthorizationEngine` trait + `PgAclEngine` + DI wiring. No services changed yet — engine is built but unused.
3. **PR 3** — service integration. Replace `verify_owner` with `authz.require` in `*_with_perms` methods. Add path-based checks. Hurl integration: `permissions.hurl` must still pass (engine's owner short-circuit ensures no behavior change for existing flows).
4. **PR 4** — REST endpoints (`/api/grants/*`) + new `grants.hurl` tests covering cross-user grant/revoke/cascade scenarios.
5. **PR 5** — `share_service` writes `access_grants` rows for new token shares (so token authz goes through the engine). At this point `storage.shares.permissions_*` columns are no longer read from anywhere — drop them.

Each PR is independently mergeable and the system stays functional throughout. PR 1-3 ship with zero observable change to users; PR 4 introduces the new feature; PR 5 retires the dead columns.

---

## Future: caching layer (in-process + Redis)

### Why

Every mutating service operation calls `authz.require(...)` at least once. The cascading SQL (`gf.lpath @> target.lpath` joined against `access_grants`) is O(log N) per check thanks to the GiST index, but at scale these costs compound:

- A batch delete of 1000 files = 1000 checks
- WebDAV PROPFIND on a deep folder may call `read` for every descendant
- A user with many active sessions hammers the same `(subject, perm, resource)` repeatedly
- Cascading means even a "no" answer requires walking the full ancestor chain — short-circuited only when the GiST index returns empty

A cache changes the cost of repeat checks from "JOIN + ltree GiST lookup" to "HashMap get" (L1) or "Redis GET" (L2). For mostly-read workloads, hit rate should be very high.

### Architecture — decorator over the trait

The `AuthorizationEngine` trait is unchanged. A `CachedAuthorizationEngine` wraps any underlying engine:

```rust
pub struct CachedAuthorizationEngine<E: AuthorizationEngine> {
    inner: E,
    l1: moka::future::Cache<DecisionKey, bool>,   // in-process, fast, per-instance
    l2: Option<Arc<dyn DistributedCache>>,         // Redis (or similar), shared across instances
}

#[derive(Hash, Eq, PartialEq, Clone)]
struct DecisionKey {
    subject: Subject,
    permission: Permission,
    resource: Resource,
}

impl<E: AuthorizationEngine> AuthorizationEngine for CachedAuthorizationEngine<E> {
    async fn check(&self, subject: Subject, perm: Permission, resource: Resource)
        -> Result<bool, DomainError>
    {
        let key = DecisionKey { subject, permission: perm, resource };

        // L1: in-process
        if let Some(decision) = self.l1.get(&key).await { return Ok(decision); }

        // L2: Redis
        if let Some(l2) = &self.l2
            && let Some(decision) = l2.get(&key).await? {
            self.l1.insert(key.clone(), decision).await;
            return Ok(decision);
        }

        // Miss — query the underlying engine and backfill
        let decision = self.inner.check(subject, perm, resource).await?;
        self.l1.insert(key.clone(), decision).await;
        if let Some(l2) = &self.l2 {
            l2.set(&key, decision, CACHE_TTL).await?;
        }
        Ok(decision)
    }

    async fn grant(...) -> Result<Grant, _> {
        let g = self.inner.grant(...).await?;
        self.invalidate_for(g.subject, g.resource).await;
        Ok(g)
    }

    async fn revoke(...) -> Result<(), _> {
        self.inner.revoke(...).await?;
        // need the affected (subject, resource) — revoke() takes only grant_id today,
        // so the trait gains a small helper or returns the deleted grant for invalidation.
        Ok(())
    }
}
```

### Three tiers worth distinguishing

1. **Per-request cache** (cheapest to ship). A `HashMap<DecisionKey, bool>` lives in a request extension. Cleared at request end. Avoids repeat checks during a single batch op (e.g., a 1000-file delete only hits the DB once per unique `(subject, perm, file)`). No invalidation problem — request scope.

2. **In-process L1** (`moka::future::Cache`). Bounded LRU with TTL. Per-server-instance. Hit on hot resources, no network. Invalidated on local `grant`/`revoke`.

3. **Distributed L2** (Redis). Shared across multiple OxiCloud server instances. Worth adding only when running multi-instance (HA / horizontal scale). Cross-instance invalidation via Redis pub/sub or short TTL.

### Invalidation — the hard part

Cascading makes per-key invalidation hard. When Alice grants Bob `read` on folder F:

- Bob's `read` on F becomes true → invalidate `(bob, read, F)`
- Bob's `read` on every descendant of F also becomes true (live cascade) → invalidate `(bob, read, child)` for every child

There's no efficient way to enumerate all descendants and invalidate each entry. Three pragmatic options:

| Strategy | Granularity | Implementation cost | Trade-off |
|---|---|---|---|
| **Subject-scoped flush** | All cached entries for `subject` regardless of resource | Cheap (one `bucket -> drop`) | Coarse — bob's checks on unrelated resources also dropped |
| **Resource-scoped flush** | All entries on `resource` and its descendants | Need to walk ltree on invalidation OR mark a "version" on the folder root | More targeted but more code |
| **Short TTL + eventual consistency** | None — wait for TTL | Trivial | Stale `true` after revoke for up to TTL seconds (bad), stale `false` after grant for up to TTL seconds (mildly annoying) |

Recommendation when this lands: subject-scoped flush as the simple default; switch to resource-scoped flush if subject churn is too painful for cache hit rate.

### Cache key normalization for cascading

Important detail: the cached entry for "bob can read folder F" doesn't need a separate entry per descendant. The engine's `check(bob, read, child)` would still go through the SQL because the cache key is `(bob, read, child)`, distinct from `(bob, read, F)`. So caching gives no descendant boost UNLESS we:

- Pre-resolve to "bob's effective grants" once (list all `(subject_id, permission, resource_id)` rows for bob) and cache that bundle, then evaluate any `check()` against the in-memory bundle. This is a classic Zanzibar-style "user list" cache.

That's a separate L1 design: cache the **bundle** of bob's grants, not individual decisions. Hit rate is high (one cached blob per active user). Invalidation is per-subject (when bob receives/loses a grant). The check becomes "is the requested resource an ltree descendant of any folder in bob's grant bundle?" — done in process, no DB round-trip.

This is probably the right L1 shape for OxiCloud given the cascade semantics.

### Config

```rust
// In OxiCloud config:
OXICLOUD_AUTHZ_CACHE=disabled  // default in v1
OXICLOUD_AUTHZ_CACHE=in_memory // L1 only
OXICLOUD_AUTHZ_CACHE=redis     // L1 + L2 (requires OXICLOUD_REDIS_URL)
OXICLOUD_AUTHZ_CACHE_TTL=300   // seconds
```

The engine selection in `common/di.rs` wraps the underlying `PgAclEngine` based on this config. Disabled by default to keep v1 minimal.

### Why this is a clean follow-up, not v1

- The `AuthorizationEngine` trait is unchanged → the cache is a pure decorator
- Owner short-circuit already avoids the DB for the most common case (caller acting on own resources) — caching's marginal value is highest only once cross-user grants are common
- Adding caching too early hides whether the uncached SQL is actually slow at production scale; better to measure first
- Redis adds a new infrastructure dependency; introducing it before there's measured pressure is premature

### When to revisit

Add per-request cache when batch ops show repeated DB checks in tracing. Add L1 in-process cache when single-instance `check` p99 latency exceeds a threshold under cross-user workloads. Add L2 Redis only when running multi-instance and cross-instance cache coherence becomes a hit-rate problem.

---

## Future (v2): extend ReBAC to calendars, address books, playlists

Three resource types already have user-to-user sharing implemented as bespoke per-feature tables. After v1 proves the engine shape on files/folders, absorb them in a follow-up plan per resource type.

### Existing share infrastructure to migrate

| Resource | Today's share table | Today's permission shape |
|---|---|---|
| Calendar (CalDAV) | `caldav.calendar_shares (calendar_id, user_id, access_level)` | `'read' | 'write' | 'owner'` |
| Address book (CardDAV) | `carddav.address_book_shares (address_book_id, user_id, can_write)` | binary `can_write` |
| Playlist (audio) | `audio.playlist_shares (playlist_id, user_id, can_write)` | binary `can_write` |

### Required changes per resource type

Each migration is small and self-contained:

1. **Schema** — extend `resource_type` CHECK constraint:
   ```sql
   ALTER TABLE storage.access_grants
       DROP CONSTRAINT access_grants_resource_type_check,
       ADD  CONSTRAINT access_grants_resource_type_check
            CHECK (resource_type IN ('folder', 'file', 'calendar', 'address_book', 'playlist'));
   ```
2. **Domain** — extend `Resource` enum with `Calendar(Uuid)`, `AddressBook(Uuid)`, `Playlist(Uuid)`.
3. **Engine** — no cascading needed (these are flat containers, not trees). The `check()` SQL becomes a simple direct lookup with no ltree join for these branches.
4. **Service refactor** — remove the bespoke `share_calendar` / `share_address_book` / `share_playlist` methods. Sharing goes through `POST /api/grants` uniformly.
5. **Cleanup triggers** — add AFTER DELETE triggers on `caldav.calendars`, `carddav.address_books`, `audio.playlists` (same pattern as v1 triggers on `storage.folders`/`storage.files`).
6. **Data migration** — convert existing share rows:
   ```sql
   INSERT INTO storage.access_grants (subject_type, subject_id, resource_type, resource_id, permission, granted_by)
   SELECT 'user', cs.user_id, 'calendar', cs.calendar_id, p.perm, c.owner_id
     FROM caldav.calendar_shares cs
     JOIN caldav.calendars c ON c.id = cs.calendar_id
     CROSS JOIN LATERAL (
       SELECT unnest(CASE cs.access_level
         WHEN 'read'  THEN ARRAY['read']
         WHEN 'write' THEN ARRAY['read','update','create','delete']
         WHEN 'owner' THEN ARRAY['read','update','create','delete','share']
       END) AS perm
     ) p;
   -- Same shape for address_book_shares (FALSE → ['read'], TRUE → ['read','update','create','delete'])
   -- Same shape for playlist_shares.
   ```
7. **Protocol mapping** (CalDAV / CardDAV only) — the WebDAV sharing properties (`<DAV:share-access>`, `<oc:invite>`) need to be re-implemented on top of the new grants. This is the largest unknown and the main reason for deferral.

### Why deferred, not in v1

- v1 must prove the `AuthorizationEngine` trait shape works before three more services land on it. If the trait needs an adjustment after running it on files, fixing it before three more migrations is much cheaper.
- The CalDAV/CardDAV protocol layer expects sharing semantics expressed via WebDAV properties — that's its own piece of work decoupled from the v1 grant table.
- Calendars/playlists are niche compared to file sharing — low migration risk if deferred.
- The 6-permission model already accommodates these without extension; the change is mechanical, just not yet.

### Suggested rollout (one PR per resource)

- **PR A** — calendars: schema constraint + Resource enum + CalendarService refactor + Hurl tests + CalDAV property mapping
- **PR B** — address books: same shape, simpler (binary `can_write`)
- **PR C** — playlists: same shape, also binary
- Each PR drops the corresponding bespoke share table at the end.

---

## Future: OpenFGA plug-in

Implementing `OpenFgaEngine` later requires:
1. Define the OpenFGA model:
   ```
   type folder
     relations
       define parent: [folder]
       define reader: [user, folder#reader]
       define creator: [user, folder#creator]
       define updater: [user, folder#updater]
       define deleter: [user, folder#deleter]
       define sharer:  [user, folder#sharer]
       define owner:   [user]
   type file
     relations
       define parent: [folder]
       define reader: [user, folder#reader]
       ...
   ```
2. On engine init, sync owner relationships (walk `storage.folders` + `storage.files`).
3. On every `grant()`, also write the tuple to OpenFGA.
4. On `check()`, query OpenFGA's `/check` endpoint.

The `AuthorizationEngine` trait shape is identical, so swapping engines is a configuration change. The PG engine remains the source of truth for `storage.access_grants` rows; OpenFGA becomes an indexed read cache.
