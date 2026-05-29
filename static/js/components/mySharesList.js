/**
 * MySharesList — row-per-grant list for the My Shares view.
 *
 * Both view modes emit one row per grant. Lane headers are emitted on
 * grouping-key change — the server guarantees ORDER BY group key first.
 *
 * Modes:
 *   'items'      — lane = resource; row identity = subject
 *   'sharedWith' — lane = user | 'links:public' | 'links:password'; row identity = resource
 */

import { formatExpiryChip } from '../core/formatters.js';
import { i18n } from '../core/i18n.js';
import { fileSharing } from '../features/sharing/fileSharing.js';
import { grants } from '../model/grants.js';
import { buildExpiryChip } from '../utils/expiryChip.js';
import { buildPasswordChip } from '../utils/passwordChip.js';
import { buildLinkChip } from './linkChip.js';
import { buildResourceIcon } from './resourceIcon.js';
import { buildRoleChip, roleLabel } from './roleChip.js';
import { createUserVignette } from './userVignette.js';

/**
 * @import {OutgoingResourceItem, OutgoingResourceGrant, FileItem, FolderItem} from '../core/types.js'
 * @typedef {'items'|'sharedWith'} ViewMode
 * @typedef {'never'|'active'|'soon'|'expired'} ExpiryState
 */

const SOON_DAYS = 30;

/**
 * @param {string|null|undefined} expiresAt
 * @returns {ExpiryState}
 */
function _expiryState(expiresAt) {
    if (!expiresAt) return 'never';
    const ms = new Date(expiresAt).getTime() - Date.now();
    if (ms < 0) return 'expired';
    if (ms <= SOON_DAYS * 86_400_000) return 'soon';
    return 'active';
}

class MySharesList {
    /**
     * @param {HTMLElement} container
     * @param {{
     *   onResourceOpen: (resource: FileItem|FolderItem, resourceType: string) => void,
     *   onShareEdit:    (resource: FileItem|FolderItem, resourceType: string) => void,
     * }} config
     */
    constructor(container, config) {
        this._container = container;
        this._config = config;
        /** @type {string|null} */
        this._lastSwimKey = null;
        /** @type {HTMLElement|null} */
        this._lastSwimEl = null;
    }

    clear() {
        this._container.innerHTML = '';
        this._lastSwimKey = null;
        this._lastSwimEl = null;
    }

    /**
     * Full re-render (page 1).
     * @param {OutgoingResourceItem[]} items
     * @param {ViewMode} viewMode
     */
    render(items, viewMode) {
        this.clear();
        this._ingest(items, viewMode);
    }

    /**
     * Cursor append (page 2+).
     * @param {OutgoingResourceItem[]} items
     * @param {ViewMode} viewMode
     */
    append(items, viewMode) {
        this._ingest(items, viewMode);
    }

    // ── Core ingest ───────────────────────────────────────────────────────────

    /**
     * @param {OutgoingResourceItem[]} items
     * @param {ViewMode} viewMode
     */
    _ingest(items, viewMode) {
        for (const item of items) {
            if (viewMode === 'items') {
                this._ingestItemsMode(item);
            } else {
                this._ingestSharedWithMode(item);
            }
        }
    }

    /**
     * Items mode — one lane per resource, one grant row per grant.
     * @param {OutgoingResourceItem} item
     */
    _ingestItemsMode(item) {
        const swimKey = `resource:${item.resource.id}`;
        const laneBody = this._ensureLane(swimKey, () => this._buildResourceLaneHeader(item));
        for (const grant of item.grants) {
            laneBody.appendChild(this._buildGrantRow(grant, item, 'items'));
        }
    }

    /**
     * SharedWith mode — one lane per user or per link bucket, one row per grant.
     * @param {OutgoingResourceItem} item
     */
    _ingestSharedWithMode(item) {
        for (const grant of item.grants) {
            let swimKey;
            if (grant.subject_type === 'user') {
                swimKey = `user:${grant.subject_id}`;
            } else if (grant.has_password) {
                swimKey = 'links:password';
            } else {
                swimKey = 'links:public';
            }
            const laneBody = this._ensureLane(swimKey, () => this._buildSubjectLaneHeader(swimKey, grant));
            laneBody.appendChild(this._buildGrantRow(grant, item, 'sharedWith'));
        }
    }

    // ── Lane management ───────────────────────────────────────────────────────

    /**
     * Return the existing lane body when swimKey matches, else create a new lane.
     * @param {string} swimKey
     * @param {() => HTMLElement} buildHeader
     * @returns {HTMLElement}
     */
    _ensureLane(swimKey, buildHeader) {
        if (swimKey === this._lastSwimKey && this._lastSwimEl) return this._lastSwimEl;

        const lane = document.createElement('div');
        lane.className = 'ms-lane';
        lane.dataset.swimKey = swimKey;

        const header = document.createElement('div');
        header.className = 'ms-lane__header';
        header.appendChild(buildHeader());
        lane.appendChild(header);

        const body = document.createElement('div');
        body.className = 'ms-lane__body';
        lane.appendChild(body);

        this._container.appendChild(lane);
        this._lastSwimKey = swimKey;
        this._lastSwimEl = body;
        return body;
    }

    /**
     * Lane header for items mode: resource icon + name link + Edit sharing button.
     * @param {OutgoingResourceItem} item
     * @returns {HTMLElement}
     */
    _buildResourceLaneHeader(item) {
        const row = document.createElement('div');
        row.className = 'ms-resource-row';

        row.appendChild(buildResourceIcon(item.resource, item.resource_type));

        const nameLink = document.createElement('a');
        nameLink.className = 'ms-resource-row__name';
        nameLink.href = '#';
        nameLink.textContent = item.resource.name;
        nameLink.addEventListener('click', (e) => {
            e.preventDefault();
            this._config.onResourceOpen(item.resource, item.resource_type);
        });
        row.appendChild(nameLink);

        const editBtn = document.createElement('button');
        editBtn.className = 'ms-resource-row__edit button ghost';
        editBtn.innerHTML = `<i class="fas fa-pencil-alt"></i> ${i18n.t('myshares.editSharing', 'Edit sharing')}`;
        editBtn.addEventListener('click', () => this._config.onShareEdit(item.resource, item.resource_type));
        row.appendChild(editBtn);

        return row;
    }

    /**
     * Lane header for sharedWith mode: user vignette or link bucket label.
     * @param {string} swimKey
     * @param {OutgoingResourceGrant} grant
     * @returns {HTMLElement}
     */
    _buildSubjectLaneHeader(swimKey, grant) {
        if (swimKey.startsWith('user:')) {
            return createUserVignette(grant.subject_id, 'list');
        }
        const el = document.createElement('div');
        el.className = 'ms-link-lane-label';
        const icon = document.createElement('i');
        if (swimKey === 'links:password') {
            icon.className = 'fas fa-lock ms-link-lane-label__icon';
            el.appendChild(icon);
            el.appendChild(document.createTextNode(` ${i18n.t('myshares.passwordLinks', 'Password-protected links')}`));
        } else {
            icon.className = 'fas fa-link ms-link-lane-label__icon';
            el.appendChild(icon);
            el.appendChild(document.createTextNode(` ${i18n.t('myshares.publicLinks', 'Public links')}`));
        }
        return el;
    }

    // ── Grant row ─────────────────────────────────────────────────────────────

    /**
     * One grant row: identity + role pill + expiry chip + ⋯ button.
     * @param {OutgoingResourceGrant} grant
     * @param {OutgoingResourceItem} item
     * @param {ViewMode} viewMode
     * @returns {HTMLElement}
     */
    _buildGrantRow(grant, item, viewMode) {
        const row = document.createElement('div');
        row.className = 'ms-grant-row';
        if (_expiryState(grant.expires_at ?? null) === 'expired') {
            row.classList.add('ms-grant-row--expired');
        }

        row.appendChild(this._buildIdentity(grant, item, viewMode));
        row.appendChild(this._buildRolePill(grant.role));
        row.appendChild(this._buildExpiryChip(grant.expires_at ?? null));
        row.appendChild(this._buildKebabBtn(grant, item, row));

        return row;
    }

    /**
     * Identity: user vignette or link icon + name; tokens in sharedWith mode add → resource.
     * @param {OutgoingResourceGrant} grant
     * @param {OutgoingResourceItem} item
     * @param {ViewMode} viewMode
     * @returns {HTMLElement}
     */
    _buildIdentity(grant, item, viewMode) {
        const el = document.createElement('div');
        el.className = 'ms-grant-row__identity';

        if (grant.subject_type === 'user' && viewMode === 'sharedWith') {
            // Lane header is already the user — show the resource instead
            el.appendChild(buildResourceIcon(item.resource, item.resource_type));
            const nameLink = document.createElement('a');
            nameLink.className = 'ms-identity__resource-name';
            nameLink.href = '#';
            nameLink.textContent = item.resource.name;
            nameLink.addEventListener('click', (e) => {
                e.preventDefault();
                this._config.onResourceOpen(item.resource, item.resource_type);
            });
            el.appendChild(nameLink);
        } else if (grant.subject_type === 'user') {
            el.appendChild(createUserVignette(grant.subject_id, 'xs'));
        } else {
            // Token — link chip handles icon + label + copy-on-click
            el.appendChild(buildLinkChip(grant));

            if (viewMode === 'sharedWith') {
                const arrow = document.createElement('span');
                arrow.className = 'ms-link-identity__arrow';
                arrow.textContent = '→';
                el.appendChild(arrow);

                const resLink = document.createElement('a');
                resLink.className = 'ms-link-identity__resource';
                resLink.href = '#';
                resLink.appendChild(buildResourceIcon(item.resource, item.resource_type));
                resLink.appendChild(document.createTextNode(` ${item.resource.name}`));
                resLink.addEventListener('click', (e) => {
                    e.preventDefault();
                    this._config.onResourceOpen(item.resource, item.resource_type);
                });
                el.appendChild(resLink);
            }
        }

        return el;
    }

    /** @param {string} role @returns {HTMLElement} */
    _buildRolePill(role) {
        return buildRoleChip(role);
    }

    /**
     * Build the expiry chip as a DOM element.
     *
     * Delegates label/tier/icon decisions to the shared `formatExpiryChip`
     * helper (used by Trash too) so all expiration chips look identical
     * across the app and stay in sync as the design evolves.
     *
     * @param {string|null} expiresAt
     * @returns {HTMLElement}
     */
    _buildExpiryChip(expiresAt) {
        const tpl = document.createElement('template');
        tpl.innerHTML = formatExpiryChip(expiresAt);
        return /** @type {HTMLElement} */ (tpl.content.firstElementChild);
    }

    // ── Kebab menu ────────────────────────────────────────────────────────────

    /**
     * @param {OutgoingResourceGrant} grant
     * @param {OutgoingResourceItem} item
     * @param {HTMLElement} rowEl
     * @returns {HTMLButtonElement}
     */
    _buildKebabBtn(grant, item, rowEl) {
        const btn = /** @type {HTMLButtonElement} */ (document.createElement('button'));
        btn.className = 'ms-kebab-btn ms-btn-icon';
        btn.setAttribute('aria-label', i18n.t('myshares.manageAccess', 'Manage access'));
        btn.innerHTML = '<i class="fas fa-ellipsis-v"></i>';
        btn.addEventListener('click', (e) => {
            e.stopPropagation();
            this._openGrantMenu(btn, grant, item, rowEl);
        });
        return btn;
    }

    /**
     * Build and show a dynamic context menu positioned below the trigger button.
     * @param {HTMLButtonElement} btn
     * @param {OutgoingResourceGrant} grant
     * @param {OutgoingResourceItem} item
     * @param {HTMLElement} rowEl
     */
    _openGrantMenu(btn, grant, item, rowEl) {
        document.querySelector('.ms-grant-menu')?.remove();

        const menu = document.createElement('div');
        menu.className = 'context-menu ms-grant-menu';

        // Current expiry as YYYY-MM-DD (or null)
        const initialExpiry = grant.expires_at ? String(grant.expires_at).slice(0, 10) : null;

        if (grant.subject_type === 'user') {
            for (const role of /** @type {('admin'|'editor'|'viewer')[]} */ (['admin', 'editor', 'viewer'])) {
                const isCurrent = grant.role === role;
                const mi = this._menuItem(isCurrent ? 'fas fa-check' : '', roleLabel(role), false, async () => {
                    menu.remove();
                    if (isCurrent) return;
                    await grants.updateRole({
                        subject: { type: grant.subject_type, id: grant.subject_id },
                        resource: { type: item.resource_type, id: item.resource.id },
                        role
                    });
                    const pill = rowEl.querySelector('.role-chip');
                    if (pill) pill.replaceWith(buildRoleChip(role));
                    grant.role = role;
                });
                if (isCurrent) mi.classList.add('ms-menu-item--current');
                menu.appendChild(mi);
            }
            menu.appendChild(this._menuSeparator());
            menu.appendChild(this._menuExpiryRow(grant, item, rowEl, initialExpiry));
            menu.appendChild(this._menuSeparator());
            menu.appendChild(
                this._menuItem('fas fa-user-times', i18n.t('myshares.removeAccess', 'Remove access'), true, async () => {
                    menu.remove();
                    await grants.revokeGrant(grant.grant_id);
                    this._removeRowAndCleanLane(rowEl);
                })
            );
        } else {
            menu.appendChild(
                this._menuItem('fas fa-copy', i18n.t('myshares.copyLink', 'Copy link'), false, async () => {
                    menu.remove();
                    const share = await fileSharing.getShareById(grant.subject_id);
                    await fileSharing.copyLinkToClipboard(share.url);
                })
            );
            menu.appendChild(this._menuSeparator());
            menu.appendChild(this._menuExpiryRow(grant, item, rowEl, initialExpiry));
            menu.appendChild(this._menuPasswordRow(grant, rowEl));
            menu.appendChild(this._menuSeparator());
            menu.appendChild(
                this._menuItem('fas fa-trash', i18n.t('myshares.deleteLink', 'Delete link'), true, async () => {
                    menu.remove();
                    await fileSharing.removeSharedLink(grant.subject_id);
                    this._removeRowAndCleanLane(rowEl);
                })
            );
        }

        document.body.appendChild(menu);

        // Position below the trigger, right-aligned to it, clamped to viewport
        const rect = btn.getBoundingClientRect();
        const mw = menu.offsetWidth || 200;
        const left = Math.min(rect.right - mw, window.innerWidth - mw - 8);
        menu.style.position = 'absolute';
        menu.style.top = `${rect.bottom + window.scrollY + 4}px`;
        menu.style.left = `${Math.max(8, left)}px`;

        const close = (/** @type {Event} */ e) => {
            if (e.type === 'keydown' && /** @type {KeyboardEvent} */ (e).key !== 'Escape') return;
            // Keep menu open when interacting with elements inside it (e.g. the date input)
            if (e.type === 'click' && menu.contains(/** @type {Node} */ (e.target))) return;
            menu.remove();
            document.removeEventListener('click', close, true);
            document.removeEventListener('keydown', close, true);
        };
        setTimeout(() => {
            document.addEventListener('click', close, true);
            document.addEventListener('keydown', close, true);
        }, 0);
    }

    /**
     * Non-closing expiry row embedded in the context menu.
     * Uses the shared smd-expiry-chip; saves on blur/Enter.
     * @param {OutgoingResourceGrant} grant
     * @param {OutgoingResourceItem} item
     * @param {HTMLElement} rowEl
     * @param {string|null} initialExpiry  YYYY-MM-DD or null
     * @returns {HTMLElement}
     */
    _menuExpiryRow(grant, item, rowEl, initialExpiry) {
        const row = document.createElement('div');
        row.className = 'ms-menu-expiry-row';

        const label = document.createElement('span');
        label.className = 'ms-menu-expiry-label';
        label.textContent = i18n.t('share.expiry', 'Expiry');
        row.appendChild(label);

        const chip = buildExpiryChip(initialExpiry, async (dateStr) => {
            const expiresIso = dateStr ? new Date(`${dateStr}T00:00:00Z`).toISOString() : null;
            try {
                await grants.updateRole({
                    subject: { type: grant.subject_type, id: grant.subject_id },
                    resource: { type: item.resource_type, id: item.resource.id },
                    role: grant.role,
                    expires_at: expiresIso
                });
                grant.expires_at = expiresIso;
                // Replace the display chip in the grant row
                const displayChip = rowEl.querySelector('.ms-expiry-chip');
                if (displayChip) {
                    const newChip = this._buildExpiryChip(expiresIso);
                    displayChip.replaceWith(newChip);
                }
            } catch (err) {
                console.error('mySharesList: setExpiry failed', err);
            }
        });
        row.appendChild(chip);

        return row;
    }

    /**
     * Non-closing password row embedded in the link context menu.
     * Saves immediately on confirm (blur / Enter).
     * @param {OutgoingResourceGrant} grant
     * @param {HTMLElement} rowEl
     * @returns {HTMLElement}
     */
    _menuPasswordRow(grant, rowEl) {
        const row = document.createElement('div');
        row.className = 'ms-menu-expiry-row';

        const label = document.createElement('span');
        label.className = 'ms-menu-expiry-label';
        label.textContent = i18n.t('share.password', 'Password');
        row.appendChild(label);

        const chip = buildPasswordChip(grant.has_password, async (newPassword) => {
            try {
                await fileSharing.updateSharedLink(grant.subject_id, {
                    password: newPassword || null
                });
                grant.has_password = !!newPassword;
                // Update the lock icon on the link chip in the row
                const linkChipEl = rowEl.querySelector('.link-chip');
                if (linkChipEl) {
                    linkChipEl.classList.toggle('link-chip--locked', grant.has_password);
                    const iconEl = linkChipEl.querySelector('.link-chip__icon');
                    if (iconEl) {
                        iconEl.className = grant.has_password ? 'fas fa-lock link-chip__icon' : 'fas fa-link link-chip__icon';
                    }
                }
            } catch (err) {
                console.error('mySharesList: setPassword failed', err);
            }
        });
        row.appendChild(chip);

        return row;
    }

    /**
     * @param {string} iconClass
     * @param {string} label
     * @param {boolean} danger
     * @param {() => void} onClick
     * @returns {HTMLElement}
     */
    _menuItem(iconClass, label, danger, onClick) {
        const el = document.createElement('div');
        el.className = danger ? 'context-menu-item context-menu-item-danger' : 'context-menu-item';
        el.setAttribute('role', 'menuitem');
        if (iconClass) {
            el.innerHTML = `<i class="${iconClass}"></i> `;
        }
        el.appendChild(document.createTextNode(label));
        el.addEventListener('click', /** @type {EventListener} */ (onClick));
        return el;
    }

    /** @returns {HTMLElement} */
    _menuSeparator() {
        const el = document.createElement('div');
        el.className = 'context-menu-separator';
        return el;
    }

    /**
     * Remove the row; if the lane body is now empty, remove the whole lane.
     * @param {HTMLElement} rowEl
     */
    _removeRowAndCleanLane(rowEl) {
        const laneBody = rowEl.closest('.ms-lane__body');
        rowEl.remove();
        if (laneBody instanceof HTMLElement && laneBody.children.length === 0) {
            const lane = laneBody.closest('.ms-lane');
            if (lane instanceof HTMLElement) {
                if (lane.dataset.swimKey === this._lastSwimKey) {
                    this._lastSwimKey = null;
                    this._lastSwimEl = null;
                }
                lane.remove();
            }
        }
    }
}

export { MySharesList };
