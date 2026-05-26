/**
 * OxiCloud – "Shared with me" view.
 *
 * Renders files and folders that other users have explicitly granted the
 * current user access to, using the cursor-paginated
 * `GET /api/grants/incoming/resources` endpoint.
 *
 * Reuses the existing `#files-list` container and `ui.renderFolders` /
 * `ui.renderFiles` so the grid ↔ list toggle and all card components work
 * out of the box. A "Load more" button is injected below the files container
 * for cursor-based pagination.
 *
 * NOTE: the grid/list container will be extracted into a reusable component
 * in a future refactor — this view is intentionally kept thin.
 */

import { ui } from '../../app/ui.js';
import { i18n } from '../../core/i18n.js';
import { multiSelect } from '../../features/files/multiSelect.js';
import { ownerTooltip } from '../../features/ownerTooltip.js';
import { grants } from '../../model/grants.js';
import { systemUsers } from '../../model/systemUsers.js';

/** @import {SharedWithMeItem, FileItem, FolderItem, ResourceTypeEnum} from '../../core/types.js' */

/** ID of the "Load more" wrapper injected below `.files-container`. */
const LOAD_MORE_ID = 'swm-load-more-wrapper';

const sharedWithMeView = {
    // ── State ─────────────────────────────────────────────────────────────────

    /** @type {string|null} */
    _nextCursor: null,

    _loading: false,

    // ── Public API ────────────────────────────────────────────────────────────

    /**
     * (Re-)load from page 1 and render into the existing files container.
     * Called every time the user switches to this section.
     */
    async init() {
        this._nextCursor = null;
        this._loading = false;

        this._ensureLoadMoreButton();

        // Start fetching system users in background so tooltips resolve instantly
        // by the time the user hovers over an item.
        systemUsers.prefetch();

        // Standard files-view setup: clear list, show container, init multiselect
        ui.resetFilesList();
        multiSelect.init();
        ui.updateBreadcrumb();

        await this._loadPage();
    },

    /**
     * Hide the "Load more" button when leaving this section.
     * The files container itself is managed by navigation.js.
     */
    hide() {
        const w = document.getElementById(LOAD_MORE_ID);
        if (w) w.classList.add('hidden');

        const filesList = document.getElementById('files-list');
        if (filesList) ownerTooltip.destroy(filesList);
    },

    // ── Internal helpers ──────────────────────────────────────────────────────

    /**
     * Fetch one page, map items → FileItem / FolderItem, render them, then
     * stamp `data-owner-id` and wire the owner tooltip.
     * @returns {Promise<void>}
     */
    async _loadPage() {
        if (this._loading) return;
        this._loading = true;

        try {
            const data = await grants.fetchSharedWithMe({
                resourceTypes: /** @type {ResourceTypeEnum[]} */ (['file', 'folder']),
                limit: 50,
                cursor: this._nextCursor ?? undefined
            });

            this._nextCursor = data.next_cursor ?? null;

            if (data.items.length === 0 && !this._nextCursor) {
                // First page came back empty
                ui.showError(`
                    <i class="fas fa-share-alt empty-state-icon"></i>
                    <p>${i18n.t('sharedwithme_emptyStateTitle', 'Nothing shared with you yet')}</p>
                    <p>${i18n.t('sharedwithme_emptyStateDesc', 'Items shared with you by other users will appear here')}</p>
                `);
                this._setLoadMoreVisible(false);
                return;
            }

            const { folders, files, ownerMap } = this._mapItems(data.items);
            if (folders.length) ui.renderFolders(folders);
            if (files.length) ui.renderFiles(files);

            // Stamp data-owner-id on the freshly-rendered cards and attach tooltips.
            const filesList = document.getElementById('files-list');
            if (filesList) {
                this._stampOwnerIds(filesList, ownerMap);
                ownerTooltip.init(filesList);
            }

            // Fill the Owner column cells (idempotent: skips already-resolved rows).
            await ui.resolveOwnerCells();

            this._setLoadMoreVisible(!!this._nextCursor);
        } catch (err) {
            ui.showError(`
                <i class="fas fa-exclamation-circle empty-state-icon error"></i>
                <p>${i18n.t('errors_loadFailed', 'Failed to load items')}</p>
            `);
            console.error('sharedWithMeView: load error', err);
        } finally {
            this._loading = false;
        }
    },

    /**
     * Map `SharedWithMeItem[]` to separate arrays for rendering plus an
     * `ownerMap` (itemId → grantedBy userId) used to stamp `data-owner-id`
     * after the cards are in the DOM.
     *
     * The backend already includes all display fields (`icon_class`,
     * `icon_special_class`, `category`, `size_formatted`) inside the nested
     * `file` / `folder` objects, so no client-side enrichment is needed.
     *
     * @param {SharedWithMeItem[]} items
     * @returns {{ folders: FolderItem[], files: FileItem[], ownerMap: Map<string,string> }}
     */
    _mapItems(items) {
        /** @type {FolderItem[]} */
        const folders = [];

        /** @type {FileItem[]} */
        const files = [];

        /** @type {Map<string, string>} itemId → grantedBy userId */
        const ownerMap = new Map();

        for (const item of items) {
            if (item.resource_type === 'folder') {
                const f = /** @type {FolderItem} */ (item.resource);
                folders.push(
                    /** @type {FolderItem} */ ({
                        id: f.id,
                        name: f.name,
                        path: f.path ?? '',
                        parent_id: f.parent_id ?? '',
                        owner_id: f.owner_id ?? '',
                        is_root: f.is_root ?? false,
                        created_at: f.created_at,
                        modified_at: f.modified_at,
                        icon_class: f.icon_class,
                        icon_special_class: f.icon_special_class ?? '',
                        category: 'folder'
                    })
                );
                ownerMap.set(f.id, item.granted_by);
            } else if (item.resource_type === 'file') {
                const f = /** @type {FileItem} */ (item.resource);
                files.push(
                    /** @type {FileItem} */ ({
                        id: f.id,
                        name: f.name,
                        path: f.path ?? '',
                        folder_id: f.folder_id ?? '',
                        owner_id: f.owner_id ?? '',
                        mime_type: f.mime_type,
                        size: f.size,
                        size_formatted: f.size_formatted,
                        created_at: f.created_at,
                        modified_at: f.modified_at,
                        sort_date: f.modified_at,
                        icon_class: f.icon_class,
                        icon_special_class: f.icon_special_class ?? '',
                        category: f.category
                    })
                );
                ownerMap.set(f.id, item.granted_by);
            }
        }

        return { folders, files, ownerMap };
    },

    /**
     * Walk `ownerMap` and set `data-owner-id` on matching `.file-item` cards
     * inside `container`.  Must be called after `renderFolders`/`renderFiles`.
     *
     * @param {HTMLElement}        container
     * @param {Map<string,string>} ownerMap  itemId → grantedBy userId
     */
    _stampOwnerIds(container, ownerMap) {
        for (const [itemId, ownerId] of ownerMap) {
            const el = container.querySelector(`[data-folder-id="${itemId}"], [data-file-id="${itemId}"]`);
            if (el instanceof HTMLElement) {
                el.dataset.ownerId = ownerId;
            }
        }
    },

    // ── "Load more" button ────────────────────────────────────────────────────

    /**
     * Create the "Load more" wrapper once and attach it below `.files-container`.
     * Subsequent calls are no-ops.
     */
    _ensureLoadMoreButton() {
        if (document.getElementById(LOAD_MORE_ID)) return;

        const filesContainer = document.querySelector('.files-container');
        if (!filesContainer) return;

        const wrapper = document.createElement('div');
        wrapper.id = LOAD_MORE_ID;
        wrapper.className = 'swm-load-more-wrapper hidden';

        const btn = document.createElement('button');
        btn.id = 'swm-load-more';
        btn.className = 'button secondary';
        btn.textContent = i18n.t('sharedwithme_loadMore', 'Load more');
        btn.addEventListener('click', () => this._loadPage());

        wrapper.appendChild(btn);
        filesContainer.after(wrapper);
    },

    /**
     * @param {boolean} visible
     */
    _setLoadMoreVisible(visible) {
        const w = document.getElementById(LOAD_MORE_ID);
        if (w) w.classList.toggle('hidden', !visible);
    }
};

export { sharedWithMeView };
