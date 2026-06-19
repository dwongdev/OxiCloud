# Resource Listing API Contract

Every OxiCloud endpoint that returns a **collection** of items must follow the conventions in
this document.  Consistency makes the REST API predictable for clients and keeps server-side
code easy to audit and extend.

## TL;DR checklist

- [ ] Response is `CursorListResponse<T>` → `{ items: T[], next_cursor?: string }`
- [ ] Query embeds `CursorQuery` via `#[serde(flatten)]`
- [ ] `limit` is clamped with `q.paging.limit_clamped()` — never trust the raw value
- [ ] Cursor is decoded with `q.paging.decode_cursor::<MyCursor>()` — invalid cursor → first page
- [ ] Cursor struct implements `PageCursor` (one bare `impl` line)
- [ ] Cursor includes **every column** in `ORDER BY` plus a unique tiebreaker (`id`)
- [ ] `sort_by` param is present even if only one sort value is meaningful today
- [ ] SQL fetches `limit + 1` rows to detect whether a next page exists

---

## Response envelope — `CursorListResponse<T>`

All listing endpoints return the same wrapper (defined in
`src/application/dtos/cursor.rs`):

```json
{
  "items": [ … ],
  "next_cursor": "eyJncmFudGVkX2F0IjoiMjAyNi…"
}
```

| Field | Type | Rules |
|---|---|---|
| `items` | `T[]` | The page of results. Length ≤ `limit`. |
| `next_cursor` | `string` | **Omitted** (not `null`) when this is the last page. |

**Never** include `total`, `page`, or `offset` — computing a total requires a `COUNT(*)` that
does not scale.

---

## Resource content field

When an item can be a **file or a folder** (or any future resource type), use a single
`resource` field rather than nullable `file`/`folder` siblings.  The existing
`resource_type` discriminator tells the client which shape to expect.

```json
{
  "resource_type": "file",
  "resource": { "id": "…", "name": "photo.jpg", "size": 204800, … },
  …
}
```

Adding a third resource type in the future only requires a new `resource_type` variant —
the wrapper shape stays the same, so older clients that don't know the new variant simply
skip the item.

### Rust — `ResourceContentDto`

```rust
#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]   // ← serialises as the inner object; no wrapper key
pub enum ResourceContentDto {
    File(FileDto),
    Folder(FolderDto),
    // Playlist(PlaylistDto),  ← add future variants here
}
```

### JavaScript / JSDoc

```js
/**
 * @typedef {Object} MyListItem
 * @property {'file'|'folder'} resource_type
 * @property {FileItem|FolderItem} resource   // always present; shape follows resource_type
 */

const f = item.resource;
if (item.resource_type === 'folder') { /* FolderItem fields */ }
else                                 { /* FileItem fields  */ }
```

---

## Standard query parameters — `CursorQuery`

`CursorQuery` (in `src/application/dtos/cursor.rs`) carries the three fields every listing
endpoint needs.  Use it directly as `Query<CursorQuery>` when there are no extra filters.
When extra params are needed, **repeat the three fields** in your endpoint-specific struct
— Axum's query extractor uses `serde_urlencoded` which does not support
`#[serde(flatten)]`:

```rust
#[derive(Debug, Deserialize, IntoParams)]
pub struct MyQuery {
    // Standard cursor fields — repeated (not flattened) due to serde_urlencoded limitation
    #[serde(default = "CursorQuery::default_limit")]
    pub limit: u32,
    pub cursor: Option<String>,
    pub sort_by: Option<String>,
    // Endpoint-specific extra
    pub status: Option<String>,
}
```

| Parameter | Type | Default | Constraints |
|---|---|---|---|
| `limit` | integer | 50 | 1–200; use `q.paging.limit_clamped()` |
| `cursor` | string | — | Opaque; absent on first page |
| `sort_by` | string | endpoint-defined | See §Sort values below |

### Naming conventions

- Use `sort_by`, **not** `order`, `orderBy`, or `sort`.
- Values are **snake_case**: `granted_at`, `name`, `size`, `granted_by`.
- Append `_desc` for descending: `name_desc`, `size_desc`.  No separate `direction` param.
- Default sort is the most natural recency order (usually `created_at DESC`).
- Return **HTTP 400** for unknown `sort_by` values.

---

## Cursor design — `PageCursor` trait

Use **keyset (seek) pagination** — never offset-based pagination.

### Why not offset?

`OFFSET N` forces the database to scan and discard N rows on every page load, which becomes
unacceptably slow for large collections.  Keyset pagination skips directly to the right row
via an index seek, regardless of page depth.

### Implementing a cursor

`PageCursor` (in `src/application/dtos/cursor.rs`) provides `encode`/`decode` as default
methods.  A cursor struct needs only a bare `impl` line:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MyCursor {
    pub created_at: DateTime<Utc>,
    pub id: Uuid,                    // tiebreaker — must be unique
}

impl PageCursor for MyCursor {}      // encode/decode for free
```

**The cursor must include every column in `ORDER BY`** plus a unique tiebreaker so that two
rows with identical sort values never cause items to be skipped or repeated.

| Sort | Cursor fields |
|---|---|
| `created_at DESC` (default) | `created_at`, `id` |
| `granted_by ASC` | `granted_by`, `created_at`, `id` |
| `name ASC` | `name`, `id` |

Encoding is URL-safe base64url (no padding) over a JSON payload — opaque to API callers.
An undecodable cursor is treated as "start from the top" (never an error).

---

## SQL implementation

Fetch **`limit + 1`** rows.  If more than `limit` rows are returned, a next page exists:
truncate to `limit` and encode the last kept item as the next cursor.

```sql
-- Default: ORDER BY created_at DESC, id DESC
WHERE (
    $cursor_created_at IS NULL                                           -- first page
    OR created_at < $cursor_created_at
    OR (created_at = $cursor_created_at AND id < $cursor_id::uuid)
)
ORDER BY created_at DESC, id DESC
LIMIT $limit + 1
```

For an additional sort dimension (e.g. `sort_by = "granted_by"`):

```sql
-- ORDER BY granted_by ASC, created_at DESC, id DESC
WHERE (
    $cursor_granted_by IS NULL
    OR granted_by > $cursor_granted_by
    OR (granted_by = $cursor_granted_by AND created_at < $cursor_created_at)
    OR (granted_by = $cursor_granted_by AND created_at = $cursor_created_at
        AND id < $cursor_id::uuid)
)
ORDER BY granted_by ASC, created_at DESC, id DESC
LIMIT $limit + 1
```

---

## Rust implementation skeleton

```rust
// ── DTO layer ────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct ThingCursor { pub created_at: DateTime<Utc>, pub id: Uuid }
impl PageCursor for ThingCursor {}

#[derive(Deserialize, IntoParams)]
pub struct ThingQuery {
    #[serde(flatten)]
    pub paging: CursorQuery,
    pub status: Option<String>,
}

// ── Handler ──────────────────────────────────────────────────────────────────

pub async fn list_things(
    Query(q): Query<ThingQuery>,
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
) -> impl IntoResponse {
    let limit  = q.paging.limit_clamped();
    let cursor = q.paging.decode_cursor::<ThingCursor>();
    let sort   = q.paging.sort_by.as_deref().unwrap_or("created_at");

    // Service returns limit+1 rows; the cursor comes from the service layer
    // (it knows which columns to include based on the sort).
    let (rows, next_cursor) = state.service
        .list_things(auth_user.id, limit + 1, cursor, sort)
        .await?;

    Json(CursorListResponse::with_cursor(
        rows.into_iter().take(limit).map(ThingDto::from).collect(),
        next_cursor.map(|c| c.encode()),
    ))
}
```

---

## JavaScript consumption pattern

```js
// static/js/core/types.js
/**
 * @template T
 * @typedef {Object} CursorListResponse
 * @property {T[]} items
 * @property {string} [next_cursor]   // absent on last page
 */

// View module (e.g. sharedWithMeView.js)
let _cursor = null;
let _loading = false;

async function loadPage() {
    if (_loading) return;
    _loading = true;
    try {
        const params = new URLSearchParams({ limit: '50' });
        if (_sortBy)  params.set('sort_by', _sortBy);
        if (_cursor)  params.set('cursor',  _cursor);

        /** @type {CursorListResponse<MyItem>} */
        const data = await fetch(`/api/things?${params}`).then(r => r.json());
        renderItems(data.items);
        _cursor = data.next_cursor ?? null;
        loadMoreBtn.hidden = _cursor === null;
    } finally {
        _loading = false;
    }
}

// Reset on section entry or sort change:
function reset() { _cursor = null; clearList(); loadPage(); }
```

---

## Sort values reference

| Value | SQL ORDER BY | Typical use |
|---|---|---|
| `created_at` (default) | `created_at DESC, id DESC` | Newest first |
| `created_at_asc` | `created_at ASC, id ASC` | Oldest first |
| `granted_by` | `granted_by ASC, created_at DESC, id DESC` | Swimlane grouping |
| `name` | `lower(name) ASC, id ASC` | Case-insensitive alpha |
| `name_desc` | `lower(name) DESC, id DESC` | Reverse alpha |
| `size` | `size_bytes ASC, id ASC` | Smallest first |
| `size_desc` | `size_bytes DESC, id DESC` | Largest first |

Only expose sort values that are meaningful for the resource type.  The `sort_by` param
must always exist in the query struct, even if only one value is supported today — this
avoids a breaking API change when a second sort is added later.

---

## Endpoint compliance

| Endpoint | Cursor | `sort_by` | Status |
|---|---|---|---|
| `GET /api/grants/incoming/resources` | ✅ | 🔜 planned | **Reference implementation** |
| `GET /api/photos` | ⚠️ `before` header | ❌ | Non-standard — migrate to body cursor |
| `GET /api/search` | ❌ offset | ✅ | Migrate cursor |
| `GET /api/folders/paginated` | ❌ page | ❌ | Migrate cursor |
| `GET /api/folders/{id}/contents/paginated` | — | — | ✅ **Removed** — use `/api/folders/{id}/resources` |
| `GET /api/admin/users` | ❌ offset | ❌ | Migrate cursor |
| `GET /api/address-books/{id}/contacts` | ❌ offset | ❌ | Migrate cursor |
| `GET /api/shares` | ❌ page | ❌ | Migrate cursor |
| `GET /api/playlists` | ❌ offset | ❌ | Migrate cursor |
| `GET /api/recent` | — | — | ✅ **Removed** — use `/api/recent/resources` |
| `GET /api/files` | ❌ **none** | ❌ | Unbounded — **urgent** |
| `GET /api/folders` | ❌ **none** | ❌ | Unbounded — **urgent** |
| `GET /api/folders/{id}/listing` | ❌ **none** | ❌ | Unbounded — **urgent** |
| `GET /api/favorites` | — | — | ✅ **Removed** — use `/api/favorites/resources` |
| `GET /api/trash` | — | — | ✅ **Removed** — use `/api/trash/resources` |
| `GET /api/grants/incoming` | ❌ **none** | ❌ | Unbounded |
| `GET /api/grants/outgoing` | ❌ **none** | ❌ | Unbounded |

---

## Migration guide — offset/none → cursor

1. **Add `CursorQuery`** via `#[serde(flatten)]` to the query struct; remove `page`,
   `offset`, `per_page`.
2. **Define a cursor struct** with the `ORDER BY` columns + `id`; add `impl PageCursor`.
3. **Adjust SQL** to the keyset `WHERE` pattern; fetch `limit + 1`.
4. **Return `CursorListResponse`** built with `from_oversized` or `with_cursor`.
5. **Remove** `total`, `total_pages`, `has_next`, `has_prev` from the response.
6. **Update the JS caller**: remove page tracking, add `_cursor` state, pass it on
   "Load more", reset to `null` on section entry.
7. **Update `types.js`**: remove old pagination typedef fields, add
   `next_cursor?: string` to the response typedef.
