# ReBAC Authorization

OxiCloud uses **Relationship-Based Access Control** (ReBAC): permissions are
expressed as a typed triple

```
Subject  has  Permission  on  Resource    (until ExpiresAt?)
```

stored as rows in a single table — `storage.access_grants` — and resolved at
request time by the **`AuthorizationEngine`** (concretely, `PgAclEngine`).

This document explains how subjects, permissions, resources, roles, groups and
two kinds of cascading fit together. For implementation details, follow the
links to the relevant Rust modules.

---

## Why ReBAC

A simpler RBAC ("Alice is an editor") is global. We need per-resource sharing:
"Alice can edit *this folder* but not that one"; "Bob can view *that file* until
March". ReBAC is the natural fit:

- **Grants are facts, not roles.** Each row is `(subject → permission → resource)`.
- **The same model covers users, anonymous share-links, groups, and federated
  identities** — they all share the `subject_type` discriminator.
- **No global "admin of folder X" magic** — the engine answers a yes/no question
  by scanning `access_grants` plus the relationships (folder ancestry, group
  membership) that connect a subject to a resource.

The owner short-circuit is the one bit of non-ReBAC logic: a resource's owner
always passes the check without a row in `access_grants`.

---

## The four entities

### Subject — *who is asking*

```rust
enum Subject {
    User(Uuid),      // auth.users
    Group(Uuid),     // auth.subject_groups
    Token(Uuid),     // storage.shares — anonymous share links
    External(Uuid),  // federated identity (Open Cloud Mesh, future)
}
```

Defined in `src/domain/services/authorization.rs`. Each variant carries the
UUID of the relevant row. The SQL discriminator (`subject_type` column) is
`'user' | 'group' | 'token' | 'external'`.

### Resource — *what is being acted on*

```rust
enum Resource {
    Folder(Uuid),
    File(Uuid),
    // Calendar / AddressBook / Playlist reserved for future use.
}
```

Both variants are content resources; the future variants will reuse the same
machinery.

### Permission — *the verb*

Six atomic permissions:

| `Read` | view the resource / list folder contents |
| `Create` | create a child resource (folders only — meaningful as inherited grant) |
| `Update` | rename, move, edit content |
| `Delete` | delete the resource |
| `Share` | grant permissions to other subjects |
| `Comment` | add comments (reserved — feature not implemented yet) |

### Role — *a named bundle of permissions*

Roles are a UX convenience that expand to permission rows server-side. There
are no role rows in the database — only permissions.

| Role | Permissions |
|---|---|
| `viewer` | `read` |
| `editor` | `read`, `comment`, `create`, `update` |
| `admin`  | `read`, `comment`, `create`, `update`, `share`, `delete` |

Defined in `src/application/dtos/grant_dto.rs::Role::expand()`. The REST API
exposes both shapes: clients can `POST /api/grants` with either `"role"` or
`"permissions"`, and `PUT /api/grants/role` reconciles the row set in one call.

---

## Storage shape

```
storage.access_grants
  id           UUID
  subject_type 'user' | 'group' | 'token' | 'external'
  subject_id   UUID
  resource_type 'folder' | 'file'
  resource_id  UUID
  permission   'read' | 'create' | 'update' | 'delete' | 'share' | 'comment'
  granted_by   UUID  (the user who issued the grant)
  granted_at   TIMESTAMPTZ
  expires_at   TIMESTAMPTZ NULL
```

One row per `(subject, permission, resource)` triple. An "owner role on folder
X for user Y" is 6 rows; a "viewer role" is 1 row.

> **Note (D-Prep, 2026-06-17):** the role assignment has since pivoted into
> a separate `storage.role_grants` table that stores **one row per role
> assignment** rather than one per permission. `access_grants` stays
> populated via dual-write during the transition; the engine reads the
> role-keyed table for authz decisions. The cleanup PR drops
> `access_grants` after the dual-write window. The historical role name
> `Admin` was renamed to `Owner` at the same time, to disambiguate from
> `UserRole::Admin` (user-account privilege) and match Drive plan
> terminology.

Cleanup is trigger-driven (`trg_cleanup_grants_folder`, …): when a resource or
subject is deleted, all referencing grants disappear in the same transaction.

---

## Subject groups — *bundling subjects*

Groups let you grant against many users at once, with two extra features:

1. **Nesting.** A group can contain users *and* other groups (up to depth 8).
   Cycles are rejected at write time by a recursive CTE in
   `subject_group_pg_repository::add_member`.

2. **Virtual groups.** Server-managed groups with a well-known UUID and
   immutable membership. Today: one entry, `Internal`
   (`00000000-…-000000000001`), implicitly containing every authenticated user.
   Future: `Everyone` (incl. externals).

The schema:

```
auth.subject_groups          (id, name, description, is_virtual, …)
auth.subject_group_members   (group_id, user_id XOR member_group_id, added_by, …)
```

Groups are addressed as a `Subject::Group(uuid)` and appear in `access_grants`
just like users. The Rust types live in
`src/domain/entities/subject_group.rs`.

---

## Two kinds of cascading

OxiCloud has **two independent cascades** that compose on every permission
check.

### 1. Resource cascade — *down the folder tree*

Folder hierarchy uses PostgreSQL `ltree`. A grant on a folder implicitly
applies to every descendant folder and to every file inside any descendant
folder. The check uses the GiST index on `storage.folders.lpath` for an
`O(log N)` ancestor lookup:

```
grant.lpath  @>  target.lpath
```

So one grant on `/projects` permits reading `/projects/q4/report.pdf`. Files
are not part of the ltree — instead, a file inherits its containing folder's
position and the cascade query joins on `target.folder_id`.

The handler-layer `_cascade_grant_exists` functions in
`src/infrastructure/services/pg_acl_engine.rs` are the canonical
implementation.

### 2. Subject cascade — *up the group tree*

A `User` caller is automatically expanded to:

```
{ user_id }  ∪  groups_for_user(user_id)  ∪  { INTERNAL_GROUP_ID }
```

where `groups_for_user` is the recursive CTE that walks
`subject_group_members` to find every group the user belongs to transitively.
A grant on the top of a nesting chain `henry ∈ B ⊂ A` permits henry to act.

The expansion is computed by `PgAclEngine::expand_user(...)` and **cached in a
Moka cache** keyed by `user_id`:

- TTL: 30 s
- Capacity: 50 000 entries
- Invalidation: TTL-only today; explicit busts on group mutation are a
  follow-up.

The cache makes the listing + cascade hot path effectively free after the
first lookup per user per ~30 s window.

### Composition

The engine combines both cascades in a single SQL round-trip:

```
SELECT 1 FROM access_grants g
  JOIN folders gf ON gf.id = g.resource_id
 WHERE g.subject_type = ANY('{user,group}')        -- subject cascade
   AND g.subject_id   = ANY($expanded_set)         --   (user + groups + Internal)
   AND g.permission   = $permission
   AND g.resource_type = 'folder'
   AND (g.expires_at IS NULL OR g.expires_at > NOW())
   AND gf.lpath @> (SELECT lpath FROM folders       -- resource cascade
                     WHERE id = $target_folder_id)
 LIMIT 1
```

The file variant adds a `UNION ALL` branch for the direct-file-grant case.

---

## How a check is decided

`PgAclEngine::check(subject, permission, resource)` returns a `bool`:

```
                ┌─── owner short-circuit ───┐
                │                           │
   subject = user, owner ⇒ Ok(true)         │
                                            ▼
       otherwise:  expand_user(uid)  ⇒  (subject_types, subject_ids)
                                            │
                                            ▼
       resource = folder:  folder_cascade_grant_exists(...)
       resource = file:    file_cascade_grant_exists(...)   (direct OR ancestor)
                                            │
                                            ▼
                                       Ok(true / false)
```

Non-user subjects (Token / External / Group-as-caller) skip the expansion —
their cascade input is a single-element set.

The decision is made entirely in the application service layer
(`*_with_perms` methods). HTTP handlers authenticate the caller and pass
`caller_id` through; they never inspect ownership or grants directly. This is
enforced by convention — see `CLAUDE.md → "Authorization (AuthZ)"`.

---

## Listing endpoints — *symmetric expansion*

The "Shared with me" feed (`GET /api/grants/incoming`, paginated
`/api/grants/incoming/resources`) reuses the same subject expansion. A user
listing their incoming grants sees both:

- Direct grants where `subject_id = caller_id`.
- Group-mediated grants where `subject_id ∈ groups_for_user(caller) ∪ {Internal}`.

This guarantees that *anything the engine would allow* also surfaces in the
listing — no silent gap between "you have access" and "you see it". The
single chokepoint is `PgAclEngine::subject_match_set(...)`, shared by `check`
and the listing queries.

The reverse direction (`/api/grants/outgoing` — "what I've shared") filters
on `granted_by = caller`. Group membership has no role there.

---

## Lifecycle

Two state machines run alongside grants:

- **Resource deletion** — folder/file delete fires a trigger
  (`trg_cleanup_grants_folder`, `trg_cleanup_grants_file`) that nukes every
  grant whose `resource_id` matches. Same transaction; clients see grants
  vanish from incoming lists immediately.
- **Subject deletion** — deleting a user or group cascades to their
  outgoing/incoming grants via FK + matching triggers.

Expiry is enforced inline: `expires_at IS NULL OR expires_at > NOW()` is part
of every cascade query, so a soft expiry doesn't need a sweeper.

---

## What ReBAC does *not* cover (yet)

The two extensions sketched in the design notes but not yet implemented:

- **`Resource::SubjectGroup(id)`** — per-group manage / use-as-subject grants.
  Would let non-admins curate their own groups, with the same engine path as
  files/folders.
- **Global roles in the JWT** (`role = "admin"`) — today these gate a few
  admin-only management endpoints (user CRUD, group CRUD). They live outside
  ReBAC because they're cross-cutting concerns, not per-resource permissions.

---

## File map

| Concern | Module |
|---|---|
| Domain types (`Subject`, `Resource`, `Permission`) | `src/domain/services/authorization.rs` |
| Subject groups (entity + repo trait) | `src/domain/entities/subject_group.rs`, `src/domain/repositories/subject_group_repository.rs` |
| Engine — `check`, listing, expansion, cache | `src/infrastructure/services/pg_acl_engine.rs` |
| Group repo — recursive CTEs, cycle/depth | `src/infrastructure/repositories/pg/subject_group_pg_repository.rs` |
| Grant DTOs + `Role::expand` | `src/application/dtos/grant_dto.rs` |
| Schema — `access_grants`, `subject_groups`, `subject_group_members` | `migrations/` |
| REST handlers | `src/interfaces/api/handlers/grant_handler.rs`, `subject_group_handler.rs` |
| Hurl coverage | `tests/api/grants.hurl`, `subject_groups.hurl`, `grants_nested_groups.hurl` |
