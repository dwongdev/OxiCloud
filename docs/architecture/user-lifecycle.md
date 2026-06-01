# User Lifecycle Hooks

Observer pattern for per-service reactions to user state transitions: created, login, logout, deleted. Mirrors the [File and Blob lifecycle](/architecture/file-and-blob-lifecycle) pattern for files; deliberately diverges from it on two points (async + sync await for most events) because user-lifecycle work is rare and sometimes has hard synchronisation requirements.

## Why hooks

Before this work, four code paths in `AuthApplicationService` each called `create_personal_folder()` immediately after inserting an `auth.users` row (public registration, first-admin bootstrap, admin-creates-user, OIDC just-in-time provisioning), plus a fifth self-heal at `folder_service.rs` for users whose folder somehow went missing. **Five places, one concern, no shared abstraction.** Adding a future per-user resource — default calendar, address book, GPG keyring, external-identity provenance for the upcoming magic-link feature — would have meant touching all five.

Hooks fix this once. Each domain service implements `UserLifecycleHook` for the events it cares about; the dispatcher fires events; services that don't care declare explicit `Ok(())` no-ops. New services register a hook in DI and inherit all four events for free.

## The trait

```rust
// src/application/ports/user_lifecycle.rs
#[async_trait]
pub trait UserLifecycleHook: Send + Sync {
    fn name(&self) -> &'static str;

    async fn on_user_created(&self, user: &User)
        -> Result<(), DomainError>;

    async fn on_user_login(&self, user: &User)
        -> Result<(), DomainError>;

    async fn on_user_logout(&self, user: &User, reason: LogoutReason)
        -> Result<(), DomainError>;

    async fn on_user_deleted(&self, user: &User, mode: DeletionMode)
        -> Result<(), DomainError>;
}
```

Two enums frame the trait:

```rust
pub enum LogoutReason {
    UserInitiated,    // explicit logout
    SessionExpired,   // TTL hit
    AdminRevoked,     // admin-initiated single-session revoke
    AccountDisabled,  // user.active flipped to FALSE → sessions revoked
    PasswordChanged,  // sibling sessions invalidated by password change
    TokenReused,      // session-family reuse detection
}

pub enum DeletionMode {
    AdminDelete,      // admin deletes via UI; resources go to trash
    GdprPurge,        // GDPR right-to-erasure; hard-delete everything
}
```

**No default impls.** Every implementor must declare all four methods explicitly. Use `Ok(())` for events you don't care about. This forces conscious acknowledgement of every lifecycle event rather than silent inheritance — same convention as `FileLifecycleHook`.

## Dispatcher semantics

`UserLifecycleService` aggregates registered hooks and fans out events with **per-event failure semantics**. The trait itself is uniform; the dispatcher decides whether to await, whether to spawn, and whether `Err` aborts.

| Event              | Awaited?         | On `Err`                                |
|--------------------|------------------|-----------------------------------------|
| `on_user_created`  | yes (sync)       | log-and-continue (retry on next login)  |
| `on_user_login`    | yes (sync)       | log-and-continue (idempotent retry)     |
| `on_user_logout`   | no (spawned)     | logged, never propagated                |
| `on_user_deleted`  | yes (sync, in tx) | **abort the transaction** — first `Err` rolls back the user DELETE and propagates to the admin endpoint as a 500 |

The asymmetry is deliberate. `on_user_created` and `on_user_login` must complete before the session token is returned, so callers see consistent state. `on_user_logout` is bookkeeping; the HTTP response shouldn't wait for cache flushes — the dispatcher spawns. `on_user_deleted` will become atomic-with-the-DELETE in PR 4 when a transaction handle joins the trait signature.

```text
            ┌──────────────────────────────────────────────────┐
            │ AuthApplicationService                           │
            │   register / login / logout / delete             │
            └──────────────────────┬───────────────────────────┘
                                   │
                                   ▼
            ┌──────────────────────────────────────────────────┐
            │ UserLifecycleService::dispatch_*                 │
            │   created / login / logout / deleted             │
            └──────────────────────┬───────────────────────────┘
                                   │
                  ┌────────────────┼────────────────┐
                  ▼                ▼                ▼
         AuditLifecycleHook  HomeFolderHook  AuthzCacheHook   …
            (PR 1 only)        (PR 3)           (PR 4)
```

## Owner-located convention

Each concrete hook impl lives **next to the service that owns the work**, not in a centralised `lifecycle/` directory.

Examples (PR plan):

- `HomeFolderLifecycleHook` lives in `src/application/services/folder_service.rs` — same module as `FolderService`, owner of home-folder policy.
- `AuthzCacheLifecycleHook` lives in `src/infrastructure/services/pg_acl_engine.rs` — same module as the Moka cache it invalidates.
- `AuditLifecycleHook` lives in `src/application/services/user_lifecycle_service.rs` (with the dispatcher) — cross-cutting, no domain owner.

This mirrors how `FileLifecycleHook` impls are placed: `ThumbnailRefreshHook` lives in `thumbnail_service.rs`, the audio metadata impl lives in `audio_metadata_service.rs`. A future maintainer reading the folder service sees the lifecycle reactions next to the rest of the folder logic — no jumping between modules to understand why a folder gets created on login.

## Tips for implementors

These are codified in the module-level docstring of `application/ports/user_lifecycle.rs` so they show up in IDE hover.

1. **First-ever login detection.** `on_user_login` fires *before* `user.register_login()` is called, so `user.last_login_at().is_none()` is a reliable "this is the user's first login since account creation" signal. Use it for welcome emails, one-shot default-resource seeding, "complete your profile" prompts.

2. **External-user short-circuit.** Hooks that provision per-user resources (home folder, default calendar, address book, GPG keys, …) must start with `if user.is_external() { return Ok(()); }`. External users (`is_external = TRUE`) are grant-only recipients — they have no home folder and don't consume storage quota. The DB `CHECK (NOT is_external OR storage_used_bytes = 0)` constraint catches code paths that bypass this short-circuit.

   **Subtle but important rule**: external users can **never** be admins. The DB enforces this via `CHECK (NOT (is_external AND role = 'admin'))`. `User::new_external(...)` doesn't accept a role parameter — it always sets `UserRole::User`. To make an existing external user an admin, an admin must first convert them to internal (`UPDATE auth.users SET is_external = FALSE`) and *then* update the role. The two-step process is intentional friction: granting admin to a federated principal would let external identity providers indirectly manage the local instance.

3. **Idempotency is mandatory.** `on_user_login` fires on every successful authentication, not just the first. A hook that creates a resource must check whether the resource already exists before creating it. Cache invalidation, audit deduplication, etc., must all tolerate redundant calls.

4. **External → internal conversion needs no special event.** When an admin flips `is_external = FALSE`, the user's next login fires `on_user_login` with the new flag value. Idempotent hooks see `!is_external` and missing resources → provision. No `on_user_converted` method needed; the safety-net pattern carries the load.

3. **Failure swallowing on create/login.** If your hook returns `Err`, the user is still created/logged in; only your hook's effect is delayed. Log enough detail via `tracing::error!` that subsequent investigation can identify the user. The next successful login's `on_user_login` will retry idempotently.

4. **Per-session logout firing.** When a flow revokes multiple sessions in one call (e.g. `revoke_all_user_sessions` on password change), today the dispatcher fires `on_user_logout` ONCE per logical revoke-call. PR 4's `SessionRevocationLifecycleHook` will refine to once-per-session for proper audit granularity. Hooks must accept N redundant calls with the same reason — keep them idempotent.

5. **`on_user_deleted` runs inside the delete transaction.** The user row still exists when the hook fires; the dispatcher commits only after every hook returns `Ok(())`. Returning `Err` aborts the whole transaction — including the user DELETE itself. Implementors get `tx: &mut sqlx::Transaction<'_, Postgres>` so cleanup queries land in the same tx (e.g. session revocation with audit trail before FK CASCADE wipes the rows). Be conservative about returning `Err`: an abort means the admin's delete operation fails, leaving the user intact.

6. **Hook order is registration order.** The DI factory determines firing sequence. If two hooks have an ordering dependency (e.g. home-folder must exist before default-calendar can be seeded inside it), the dependent hook registers AFTER the producer. Document the convention inline in the DI block.

## Concrete hooks shipped today

| Hook | Lives in | Responsibility |
|---|---|---|
| `AuditLifecycleHook` | `src/application/services/user_lifecycle_service.rs` (co-located with dispatcher) | All four events: emits one `tracing::info!(target: "audit", event = "user.*", ...)` per call, with `is_external` as a field. Co-located because audit is cross-cutting with no domain owner. |
| `HomeFolderLifecycleHook` | `src/application/services/folder_service.rs` (same module as `FolderService`) | `on_user_created` + `on_user_login`: idempotently provision "My Folder - {username}" via `FolderService::ensure_home_folder`. Short-circuits when `user.is_external()`. `on_user_logout`: `Ok(())`. `on_user_deleted`: per-mode `tracing::info!` event so audit distinguishes AdminDelete from GdprPurge — the FK CASCADE on `storage.folders.user_id` handles the actual row removal. Trash-with-retention is documented as future work. |
| `AuthzCacheLifecycleHook` | `src/infrastructure/services/pg_acl_engine.rs` (same module as the Moka cache it invalidates) | `on_user_logout` + `on_user_deleted`: `engine.invalidate_user_groups_cache(user.id())` — drops the cached transitive-group expansion immediately so a re-login (or a re-created account with the same id) sees fresh memberships without waiting for the 30 s TTL. `on_user_created` + `on_user_login`: `Ok(())` (no stale entry could exist for these). |
| `SessionRevocationLifecycleHook` | `src/application/services/user_lifecycle_service.rs` (co-located with dispatcher; no dedicated session service module today) | `on_user_deleted`: explicit `session_storage.revoke_all_user_sessions(user.id())` + aggregate audit event (`event = "user.sessions_revoked_on_delete", count = N`). Replaces the silent FK CASCADE with an observable revocation. All other events: `Ok(())`. |
| `DeletionMode` | enum on the trait | Distinguishes admin-initiated delete (`AdminDelete` — currently identical to GDPR but reserved for a future trash-with-retention policy) from GDPR right-to-erasure purge (`GdprPurge` — for a future sweeper). PR 4 ships the variants; future PRs may add per-mode behaviour. |
| `ExternalIdentityLifecycleHook` *(no-op stub)* | `src/application/services/external_identity_service.rs` (own module — the future home of the magic-link / OIDC / OCM provenance service) | All four methods are explicit `Ok(())` today. The magic-link PR sequence will populate them: `on_user_created` will INSERT into the future `auth.user_external_identity` side-table for `is_external` users; `on_user_login` will bump `last_verified_at` for GDPR-sweeper purposes; `on_user_logout` and `on_user_deleted` will stay no-ops (FK CASCADE handles row removal). The stub lands now so the magic-link PR fills in hook bodies without touching DI registration. |

### How the delete transaction composes

```text
AuthApplicationService::delete_user_admin(user_id)
    │
    ▼
BEGIN
    │
    ▼
dispatch_deleted(user, AdminDelete, &mut tx)
    │
    ├── AuditLifecycleHook        → tracing::info!(event="user.deleted", mode=...)
    ├── HomeFolderLifecycleHook   → tracing::info!("home folder will be removed via FK CASCADE")
    ├── AuthzCacheLifecycleHook   → engine.invalidate_user_groups_cache(user_id)
    └── SessionRevocationLifecycleHook
                                  → session_storage.revoke_all_user_sessions(user_id)
                                  → tracing::info!(event="user.sessions_revoked_on_delete",
                                                   count=N)
    │
    ▼
DELETE FROM auth.users WHERE id = $1
    │
    ▼  (FK CASCADE removes folders, files, app_passwords, device_codes, calendar
    │   shares, address-book shares, etc.; trg_cleanup_grants_user removes the
    │   user's grants)
    │
    ▼
COMMIT
```

If any hook returns `Err`, the dispatcher propagates it; `delete_user_admin` rolls back the transaction and surfaces the error to the admin endpoint as a 500. The user row, all sessions, all folders/files, all grants — everything stays in place. Hook implementors should keep that strong constraint in mind: returning `Err` from `on_user_deleted` is a heavy hammer.

### Worked example: brand-new user logs in for the first time

1. Client POSTs `/api/auth/login` with valid credentials.
2. `AuthApplicationService::login()` validates the password against the stored Argon2 hash.
3. **Before** `user.register_login()` is called, the dispatcher fires `dispatch_login(&user)`. The user's `last_login_at` is still `None` from creation time.
4. `AuditLifecycleHook::on_user_login` runs first (registration order): emits `event = "user.login", user_id = ..., username = ..., is_external = false, first_login = true`.
5. `HomeFolderLifecycleHook::on_user_login` runs next: sees `!user.is_external()`, calls `FolderService::ensure_home_folder(uid, username)`. The service checks `list_folders_by_owner(None, uid)` — empty → creates `"My Folder - alice"`. Returns `Ok(true)` (newly created).
6. Dispatcher finishes. `user.register_login()` is now called, stamping `last_login_at` to the current time.
7. The session row is INSERTed; access + refresh tokens generated; response returned to the client.

On the user's **second** login: same flow up through step 5, but `ensure_home_folder` finds the existing folder, returns `Ok(false)`, no-op. The `AuditLifecycleHook` still emits an event, but `first_login = false` this time.

If the home folder gets deleted manually (e.g., SQL `DELETE FROM storage.folders WHERE user_id = $1`), the user's **next** login will re-create it — that's the safety-net behaviour the lifecycle hook contractually owns.

## Future events (NOT shipped — design door)

These events are reserved for situations that don't exist yet but probably will. Adding a method to the trait costs every hook impl a new no-op forever, so we don't add them speculatively. Each row lists what would force the addition.

| Future event | Why someone might want it | What would force adding it |
|---|---|---|
| `on_user_password_changed` | Notify the user via email; invalidate cached credentials; trigger TOTP re-enrolment | A per-user notification service. Today the existing `revoke_all_user_sessions` cascade fires `on_user_logout(PasswordChanged)` for each session — sufficient for current consumers. |
| `on_user_role_changed` | Audit promotion to admin; revoke admin-only sessions on demotion | A multi-role system. Today only `admin` / `user` exist and the one-liner audit log at the admin handler covers it. |
| `on_user_email_changed` | External users: re-verify the new email via magic-link; notify both old and new addresses | When external users start changing their email. Today email is immutable. |
| `on_user_avatar_changed` | Bust thumbnail caches; sync to federated servers (OCM) | When OCM federation ships. |
| `on_user_disabled` / `on_user_enabled` | Audit-distinguishable state changes; pause per-user scheduled jobs | When per-user scheduled jobs land. Today `on_user_logout(AccountDisabled)` covers the only consumer. |
| `on_user_external_to_internal_converted` | Welcome email; pre-provision internal-only resources at conversion time | If admins routinely promote external users and the next-login lag is unacceptable. Today idempotent `on_user_login` handles conversion fine. |
| `on_user_2fa_enabled` / `on_user_2fa_disabled` | Audit; force re-login of other sessions | When 2FA ships. |

**Rule of thumb for adding any of these**: pair the addition with a default `Ok(())` body (one-time exception to the "no defaults" rule) so existing hooks don't need to declare it. State in the docstring whether the event is await-or-spawn and whether `Err` aborts.

## File map

| Concern | Module |
|---|---|
| Trait + `LogoutReason` + `DeletionMode` enums + tips | `src/application/ports/user_lifecycle.rs` |
| Dispatcher + `AuditLifecycleHook` | `src/application/services/user_lifecycle_service.rs` |
| Wire-in: created / login / logout / deleted | `src/application/services/auth_application_service.rs` |
| DI registration | `src/common/di.rs` (constructs the dispatcher) + `src/infrastructure/auth_factory.rs` (threads it into `AuthApplicationService`) |
