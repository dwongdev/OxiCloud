// @ts-check

/**
 * OxiCloud – Trash resources model.
 *
 * Thin fetch wrapper for `GET /api/trash/resources` (cursor-paginated).
 * The legacy `GET /api/trash` endpoint is deprecated server-side and is no
 * longer called from the UI.
 */

/** @import {FileItem, FolderItem, ResourceTypeEnum} from '../core/types.js' */

/**
 * @typedef {Object} TrashResourceItem
 * @property {ResourceTypeEnum}    resource_type  - 'file' | 'folder'
 * @property {string}              trashed_at     - ISO-8601: when the item was sent to trash
 * @property {string}              deletion_date  - ISO-8601: when retention will purge it
 * @property {FileItem|FolderItem} resource       - Full resource details (resource.path = original location)
 */

/**
 * @typedef {Object} TrashResourcesResponse
 * @property {TrashResourceItem[]}  items
 * @property {string|undefined}     [next_cursor]
 */

/**
 * Fetch one page of the current user's trashed resources.
 *
 * @param {{
 *   cursor?:        string,
 *   orderBy?:       string,
 *   limit?:         number,
 *   reverse?:       boolean,
 *   resourceTypes?: ResourceTypeEnum[],
 * }} [opts]
 * @returns {Promise<TrashResourcesResponse>}
 */
async function fetchTrashPage({ cursor, orderBy = 'deletion_date', limit = 50, reverse = false, resourceTypes } = {}) {
    const params = new URLSearchParams({ order_by: orderBy, limit: String(limit) });
    if (cursor) params.set('cursor', cursor);
    if (reverse) params.set('reverse', 'true');
    if (resourceTypes?.length) params.set('resource_types', resourceTypes.join(','));

    const res = await fetch(`/api/trash/resources?${params}`, {
        credentials: 'same-origin',
        cache: 'no-store'
    });

    if (!res.ok) {
        const err = /** @type {any} */ (new Error(`GET /api/trash/resources failed: ${res.status}`));
        err.status = res.status;
        throw err;
    }

    return /** @type {Promise<TrashResourcesResponse>} */ (res.json());
}

export { fetchTrashPage };
