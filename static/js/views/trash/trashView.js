// @ts-check

/**
 * OxiCloud – Trash view.
 *
 * Renders the user's trashed files and folders using the cursor-paginated
 * `GET /api/trash/resources` endpoint. Default sort is by `deletion_date` ASC
 * (items expiring soonest first) with group-by "remaining days" — the user's
 * primary concern in this section.
 *
 * Public API mirrors `recentView`:
 *   - `groupByDefs`            — array of group-by dimension definitions
 *   - `setGroupBy(key)`        — change active dimension + reload from page 1
 *   - `setDirection(reversed)` — flip sort direction + reload from page 1
 *   - `init()`                 — (re-)enter the section; restores prefs + loads page 1
 *   - `hide()`                 — called when leaving this section
 */

import { ui } from '../../app/ui.js';
import { ResourceListComponent } from '../../components/resourceList.js';
import { formatExpiryChip, normalizeDateBucket, normalizeExpiryBucket, sizeBucket } from '../../core/formatters.js';
import { i18n } from '../../core/i18n.js';
import * as viewPrefs from '../../core/viewPrefs.js';
import { fileOps } from '../../features/files/fileOperations.js';
import * as itemTooltip from '../../features/itemTooltip.js';
import { fetchTrashPage } from '../../model/trashModel.js';

/** @import {FileItem, FolderItem, ResourceTypeEnum, TrashResourceItem} from '../../core/types.js' */

/**
 * @typedef {{ key: string, label: string, orderBy: string, reverseDefault?: boolean,
 *             keyFn: (item: FileItem|FolderItem) => string|null,
 *             labelFn?: (key: string) => string,
 *             headerNodeFn?: (key: string) => HTMLElement }} GroupByDef
 */

/**
 * Group-by dimension definitions for the Trash section.
 *
 * The default `remainingDays` mode answers the user's most-asked question:
 * "what's about to be deleted?" It orders by `deletion_date` ASC (soonest first)
 * and groups via the existing `normalizeExpiryBucket` aggregator
 * ("Tomorrow", "In less than 7 days", "In less than 30 days", …).
 *
 * `remainingDays` and `trashedTime` both touch the timestamp axis but use
 * distinct server `orderBy` values so the API is self-documenting and the
 * defaults can differ (ASC vs DESC).
 *
 * @type {GroupByDef[]}
 */
const GROUP_BY_DEFS = [
    {
        key: 'remainingDays',
        get label() {
            return i18n.t('trash.groupby.remaining_days', 'Remaining days');
        },
        orderBy: 'deletion_date',
        reverseDefault: false,
        keyFn: (item) => {
            const r = /** @type {Record<string,string>} */ (/** @type {unknown} */ (item));
            return r.deletion_date ? normalizeExpiryBucket(r.deletion_date) : null;
        }
    },
    {
        key: 'type',
        get label() {
            return i18n.t('groupby.type', 'Type');
        },
        orderBy: 'type',
        reverseDefault: false,
        keyFn: (item) => ('mime_type' in item ? /** @type {Record<string,string>} */ (/** @type {unknown} */ (item)).category || 'other' : 'Folder'),
        labelFn: (key) => {
            // biome-ignore format: keep indentation
            /** @type {Record<string, string>} */
            const labels = {
                Folder:       i18n.t('groupby.type.folders',     'Folders'),
                Image:        i18n.t('category.images',          'Images'),
                Video:        i18n.t('category.videos',          'Videos'),
                Audio:        i18n.t('category.audio',           'Audio'),
                PDF:          'PDF',
                Document:     i18n.t('category.documents',       'Documents'),
                Spreadsheet:  i18n.t('category.spreadsheets',    'Spreadsheets'),
                Presentation: i18n.t('category.presentations',   'Presentations'),
                Archive:      i18n.t('category.archives',        'Archives'),
                Code:         i18n.t('category.code',            'Code'),
                Markdown:     i18n.t('category.markdown',        'Markdown'),
                Text:         i18n.t('category.text',            'Text'),
                Installer:    i18n.t('category.installers',      'Installers')
            };
            return labels[key] ?? key;
        }
    },
    {
        key: 'size',
        get label() {
            return i18n.t('groupby.size', 'Size');
        },
        orderBy: 'size',
        reverseDefault: false,
        keyFn: (item) => {
            if (!('mime_type' in item)) return sizeBucket(-1);
            const r = /** @type {Record<string,number>} */ (/** @type {unknown} */ (item));
            return sizeBucket(r.size ?? 0);
        }
    },
    {
        key: 'trashedTime',
        get label() {
            return i18n.t('trash.groupby.trashed_time', 'Trashed time');
        },
        orderBy: 'trashed_at',
        reverseDefault: false,
        keyFn: (item) => {
            const r = /** @type {Record<string,string>} */ (/** @type {unknown} */ (item));
            return r.trashed_at ? normalizeDateBucket(r.trashed_at) : null;
        }
    }
];

/** ID of the "Load more" wrapper injected below `.files-container`. */
const LOAD_MORE_ID = 'trash-load-more-wrapper';

const trashView = {
    // ── State ─────────────────────────────────────────────────────────────────

    /** @type {string|null} */
    _nextCursor: null,

    _loading: false,

    /** @type {ResourceListComponent|null} */
    _component: null,

    /**
     * Active group-by key. Default is `'remainingDays'` — items expiring soonest first.
     * @type {string}
     */
    _groupBy: 'remainingDays',

    /** Whether the current sort order is reversed. */
    _reversed: false,

    // ── Public API ────────────────────────────────────────────────────────────

    /**
     * The group-by dimension definitions for this section.
     * `main.js` reads this to populate the Group-by dropdown dynamically.
     * @returns {GroupByDef[]}
     */
    get groupByDefs() {
        return GROUP_BY_DEFS;
    },

    /**
     * Change the active group-by dimension and reload from page 1.
     * Calling with the current key is a no-op.
     * @param {string} key
     */
    setGroupBy(key) {
        if (this._groupBy === key) return;
        this._groupBy = key;
        viewPrefs.save('trash', this._groupBy, this._reversed, viewPrefs.load('trash').view);
        this._nextCursor = null;
        this._component?.clear();
        this._loadPage();
    },

    /**
     * Flip the sort direction and reload from page 1.
     * Calling with the current value is a no-op.
     * @param {boolean} reversed
     */
    setDirection(reversed) {
        if (this._reversed === reversed) return;
        this._reversed = reversed;
        viewPrefs.save('trash', this._groupBy, this._reversed, viewPrefs.load('trash').view);
        this._nextCursor = null;
        this._component?.clear();
        this._loadPage();
    },

    /**
     * (Re-)enter the Trash section: restore saved prefs, create / reuse the
     * component, and load page 1.
     */
    async init() {
        this._nextCursor = null;
        this._loading = false;
        const savedPrefs = viewPrefs.load('trash');
        this._groupBy = savedPrefs.groupBy || 'remainingDays';
        this._reversed = savedPrefs.reversed;

        this._ensureLoadMoreButton();

        ui.resetFilesList();
        ui.updateBreadcrumb();

        const filesList = document.getElementById('files-list');
        if (filesList) {
            // Marker class so trash-specific CSS (column widths, corner badge)
            // can scope itself without :has() and is removed when navigating away.
            filesList.classList.add('trash-list');
            // Replace the generic header with a trash-specific one so the
            // column labels match what ResourceList actually renders
            // (Name → Path → Size → Date → Actions; no checkbox, no owner, no type).
            const header = filesList.querySelector('.list-header');
            if (header) {
                header.classList.add('trash-header');
                header.innerHTML = `
                    <div data-i18n="files.name">${i18n.t('files.name', 'Name')}</div>
                    <div data-i18n="trash.original_location">${i18n.t('trash.original_location', 'Original location')}</div>
                    <div data-i18n="files.size">${i18n.t('files.size', 'Size')}</div>
                    <div data-i18n="trash.remaining">${i18n.t('trash.remaining', 'Remaining')}</div>
                    <div></div><!-- actions -->
                `;
            }

            if (!this._component) {
                this._component = new ResourceListComponent(/** @type {HTMLElement} */ (filesList), {
                    selectable: false,
                    showFavorite: false,
                    showOwner: false,
                    showShareBadge: false,
                    showContextMenu: false,
                    showType: false,
                    showPath: true,
                    draggable: false,
                    // Show the *remaining lifetime* before retention purges the item
                    // ("In 27 days", "Tomorrow", "Expired") rather than a raw
                    // timestamp — that is what the user actually wants to know in
                    // this section. The "trashed time" is still available via the
                    // trashedTime groupBy header.
                    dateField: 'deletion_date',
                    dateLabel: 'trash.deleted_date',
                    dateFormatter: formatExpiryChip,
                    customActions: [
                        {
                            iconHtml: '<i class="fas fa-undo"></i>',
                            labelKey: 'trash.restore',
                            className: 'btn-action--restore',
                            onClick: async (item) => {
                                if (await fileOps.restoreFromTrash(item.id)) {
                                    await this._reloadFromTop();
                                }
                            }
                        },
                        {
                            iconHtml: '<i class="fas fa-trash"></i>',
                            labelKey: 'trash.delete_permanently',
                            className: 'btn-action--delete',
                            onClick: async (item) => {
                                if (await fileOps.deletePermanently(item.id)) {
                                    await this._reloadFromTop();
                                }
                            }
                        }
                    ]
                });
            }
        }

        await this._loadPage();
    },

    /**
     * Hide the "Load more" button when leaving this section.
     */
    hide() {
        const w = document.getElementById(LOAD_MORE_ID);
        if (w) w.classList.add('hidden');

        const filesList = document.getElementById('files-list');
        if (filesList) {
            filesList.classList.remove('trash-list');
            itemTooltip.destroy(filesList);
        }
    },

    // ── Internal helpers ──────────────────────────────────────────────────────

    /**
     * Discard the current page state and re-fetch from the start.
     * Used after restore / permanent-delete to refresh the visible set.
     * @returns {Promise<void>}
     */
    async _reloadFromTop() {
        this._nextCursor = null;
        this._component?.clear();
        await this._loadPage();
    },

    /**
     * Fetch one page, map items → FileItem / FolderItem, render them.
     * @returns {Promise<void>}
     */
    async _loadPage() {
        if (this._loading) return;
        this._loading = true;

        const isFirstPage = this._nextCursor === null;

        try {
            const def = GROUP_BY_DEFS.find((d) => d.key === this._groupBy);
            const orderBy = def?.orderBy ?? 'deletion_date';

            const data = await fetchTrashPage({
                resourceTypes: /** @type {ResourceTypeEnum[]} */ (['file', 'folder']),
                limit: 50,
                cursor: this._nextCursor ?? undefined,
                orderBy,
                reverse: this._reversed
            });

            this._nextCursor = data.next_cursor ?? null;

            if (data.items.length === 0 && isFirstPage) {
                ui.showError(`
                    <i class="fas fa-trash empty-state-icon"></i>
                    <p>${i18n.t('trash.empty_state', 'Trash is empty')}</p>
                `);
                this._setLoadMoreVisible(false);
                return;
            }

            const items = this._mapItems(data.items);

            if (isFirstPage) {
                this._component?.render(items, def?.keyFn, def?.labelFn, def?.headerNodeFn);
            } else {
                this._component?.append(items, def?.keyFn, def?.labelFn, def?.headerNodeFn);
            }

            // Wire unified item tooltip (owner + path) after items are in the DOM.
            const filesList = document.getElementById('files-list');
            if (filesList) itemTooltip.init(filesList);

            this._setLoadMoreVisible(!!this._nextCursor);
        } catch (err) {
            ui.showError(`
                <i class="fas fa-exclamation-circle empty-state-icon error"></i>
                <p>${i18n.t('errors_loadFailed', 'Failed to load items')}</p>
            `);
            console.error('trashView: load error', err);
        } finally {
            this._loading = false;
        }
    },

    /**
     * Map `TrashResourceItem[]` → a flat `(FileItem|FolderItem)[]` preserving
     * server order. Stamps `trashed_at` and `deletion_date` onto each item so
     * the date column and the `remainingDays` / `trashedTime` keyFns can read
     * them directly.
     *
     * @param {TrashResourceItem[]} items
     * @returns {Array<FileItem|FolderItem>}
     */
    _mapItems(items) {
        /** @type {Array<FileItem|FolderItem>} */
        const result = [];

        for (const item of items) {
            if (item.resource_type === 'folder') {
                const f = /** @type {FolderItem} */ (item.resource);
                result.push(
                    /** @type {FolderItem} */ ({
                        id: f.id,
                        name: f.name,
                        path: f.path ?? '',
                        parent_id: f.parent_id ?? '',
                        owner_id: f.owner_id ?? '',
                        is_root: f.is_root ?? false,
                        created_at: f.created_at,
                        modified_at: f.modified_at,
                        // Stamp trash-specific timestamps so the date column +
                        // remainingDays/trashedTime keyFns can read them.
                        trashed_at: item.trashed_at,
                        deletion_date: item.deletion_date,
                        icon_class: f.icon_class,
                        icon_special_class: f.icon_special_class ?? '',
                        category: 'Folder'
                    })
                );
            } else if (item.resource_type === 'file') {
                const f = /** @type {FileItem} */ (item.resource);
                result.push(
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
                        // sort_date is required by FileItem but unused in Trash —
                        // we group by trashed_at / deletion_date instead.
                        sort_date: 0,
                        trashed_at: item.trashed_at,
                        deletion_date: item.deletion_date,
                        icon_class: f.icon_class,
                        icon_special_class: f.icon_special_class ?? '',
                        category: f.category
                    })
                );
            }
        }

        return result;
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
        btn.id = 'trash-load-more';
        btn.className = 'button secondary';
        btn.textContent = i18n.t('recent.loadMore', 'Load more');
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

export { trashView };
