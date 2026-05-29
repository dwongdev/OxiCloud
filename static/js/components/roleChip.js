// @ts-check

/**
 * roleChip — shared role indicator (`Can manage` / `Can edit` / `Can view`).
 *
 * Returns the same chip HTML / element used by My Shares and any other
 * surface that needs to display a permission role. Three states:
 *
 *   admin  → "Can manage" — crown icon, orange palette
 *   editor → "Can edit"   — pencil icon, blue palette
 *   viewer → "Can view"   — eye icon, neutral palette
 *
 * CSS lives in `components/roleChip.css`.
 */

import { escapeHtml } from '../core/formatters.js';
import { i18n } from '../core/i18n.js';

/**
 * Translate a role identifier into the modifier suffix used for the chip's
 * CSS class (`role-chip--<mod>`). Unknown roles default to `view`.
 * @param {string} role
 * @returns {'manage'|'edit'|'view'}
 */
function roleMod(role) {
    if (role === 'admin') return 'manage';
    if (role === 'editor') return 'edit';
    return 'view';
}

/**
 * Translate a role identifier into a localized human-readable label.
 * Exported so callers that just want the label (e.g. context-menu rows)
 * can reuse the same wording the chip uses.
 * @param {string} role
 * @returns {string}
 */
export function roleLabel(role) {
    /** @type {Record<string,string>} */
    const m = {
        admin: i18n.t('share.role.canManage', 'Can manage'),
        editor: i18n.t('share.role.canEdit', 'Can edit'),
        viewer: i18n.t('share.role.canView', 'Can view')
    };
    return m[role] ?? role;
}

/**
 * Map a role to its FontAwesome icon class.
 * @param {string} role
 * @returns {string}
 */
function roleIcon(role) {
    if (role === 'admin') return 'fa-crown';
    if (role === 'editor') return 'fa-pencil-alt';
    return 'fa-eye';
}

/**
 * Render the role chip as an HTML snippet.
 * @param {string} role
 * @returns {string}
 */
export function formatRoleChip(role) {
    const mod = roleMod(role);
    const icon = roleIcon(role);
    const label = roleLabel(role);
    return `<span class="role-chip role-chip--${mod}"><i class="fas ${icon} role-chip__icon"></i>${escapeHtml(label)}</span>`;
}

/**
 * Build the role chip as a DOM element. Convenience for callers that need
 * an Element (e.g. `row.appendChild(...)`).
 * @param {string} role
 * @returns {HTMLElement}
 */
export function buildRoleChip(role) {
    const tpl = document.createElement('template');
    tpl.innerHTML = formatRoleChip(role);
    return /** @type {HTMLElement} */ (tpl.content.firstElementChild);
}
