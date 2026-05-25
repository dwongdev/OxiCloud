// @ts-check

/**
 * ShareModal — unified sharing dialog for files and folders.
 *
 * Covers two areas:
 *   • People (user-to-user grants via `/api/grants`)
 *   • Public links (via `/api/shares`)
 *
 * All mutations are staged locally and committed only when the user clicks
 * Apply.  The only immediate action is Copy Link (clipboard).
 *
 * The dialog shell (overlay, animation, header, footer, Escape/click-outside
 * handling) is delegated entirely to `Modal.openPanel()`.
 */

import { ui } from '../app/ui.js';
import { i18n } from '../core/i18n.js';
import { fileSharing } from '../features/sharing/fileSharing.js';
import { addressBook, SYSTEM_BOOK_ID } from '../model/addressBook.js';
import { grants } from '../model/grants.js';
import { systemUsers } from '../model/systemUsers.js';
import { Modal } from './modal.js';

/** @import {FileItem, FolderItem, Grant, ContactItem, MemberEntry, LinkEntry, DraftLink, ShareRoleEnum} from '../core/types.js' */

// ── Helpers ────────────────────────────────────────────────────────────────────

/** Permissions that belong to each role (must mirror the Rust DTO). */
const ROLE_PERMISSIONS = {
    viewer: ['read'],
    editor: ['read', 'comment', 'create', 'update'],
    admin: ['read', 'comment', 'create', 'update', 'share', 'delete']
};

/**
 * Derive the highest role a set of grants represents for one subject.
 * @param {Grant[]} subjectGrants
 * @returns {ShareRoleEnum}
 */
function _roleFromGrants(subjectGrants) {
    const perms = new Set(subjectGrants.map((g) => g.permission));
    if (perms.has('delete') || perms.has('share')) return 'admin';
    if (perms.has('create') || perms.has('update')) return 'editor';
    return 'viewer';
}

/**
 * Group grants by subject id and return one MemberEntry per unique subject.
 * @param {Grant[]} grantList
 * @returns {MemberEntry[]}
 */
function _buildMembers(grantList) {
    /** @type {Map<string, Grant[]>} */
    const bySubject = new Map();
    for (const g of grantList) {
        // Token grants represent public-link access — they belong in the Links
        // section, not the People section.
        if (g.subject.type === 'token') continue;
        const key = g.subject.id;
        if (!bySubject.has(key)) bySubject.set(key, []);
        bySubject.get(key).push(g);
    }
    /** @type {MemberEntry[]} */
    const members = [];
    for (const subjectGrants of bySubject.values()) {
        members.push({
            grant: subjectGrants[0], // representative grant (used for subject info)
            role: _roleFromGrants(subjectGrants),
            _op: 'keep'
        });
    }
    return members;
}

/**
 * Get initials for an avatar (up to 2 chars).
 * @param {string} name
 * @returns {string}
 */
function _initials(name) {
    const parts = name.trim().split(/\s+/);
    if (parts.length >= 2) return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase();
    return name.slice(0, 2).toUpperCase();
}

// ── Component ──────────────────────────────────────────────────────────────────

const shareModal = {
    // ── State ──────────────────────────────────────────────────────────────────

    /** @type {FileItem|FolderItem|null} */
    _item: null,

    /** @type {'file'|'folder'} */
    _itemType: 'file',

    /** @type {MemberEntry[]} */
    _localMembers: [],

    /** @type {LinkEntry[]} */
    _localLinks: [],

    /** @type {DraftLink[]} */
    _newLinks: [],

    /** @type {ContactItem[]} */
    _stagedUsers: [],

    /** @type {ShareRoleEnum} */
    _stagedRole: 'viewer',

    /** @type {HTMLElement|null} — body node injected into Modal */
    _bodyEl: null,

    // ── Public API ─────────────────────────────────────────────────────────────

    /**
     * Open the share modal for a file or folder.
     * @param {FileItem|FolderItem} item
     * @param {'file'|'folder'}     itemType
     */
    async open(item, itemType) {
        this._item = item;
        this._itemType = itemType;
        this._localMembers = [];
        this._localLinks = [];
        this._newLinks = [];
        this._stagedUsers = [];
        this._stagedRole = 'viewer';

        const title = `${i18n.t('share.shareOf', 'Share of:')} ${item.name}`;

        // Build body with loading skeleton
        this._bodyEl = this._buildSkeleton();

        Modal.openPanel({
            title,
            icon: 'fa-share-alt',
            content: this._bodyEl,
            confirmText: i18n.t('actions.apply', 'Apply'),
            onConfirm: () => {
                this._applyAll();
            } // intentionally discard Promise
        });

        // Prefetch system users in background so tooltips resolve instantly.
        systemUsers.prefetch();

        // Load data
        try {
            const [grantList, linkList] = await Promise.all([
                grants.fetchGrantsForResource(itemType, item.id),
                fileSharing.getSharedLinksForItem(item.id, itemType)
            ]);

            this._localMembers = _buildMembers(grantList);
            this._localLinks = linkList.map((share) => /** @type {LinkEntry} */ ({ share, _op: 'keep', _draft: null }));
        } catch (err) {
            console.error('shareModal: load error', err);
        }

        // Swap skeleton → real content
        if (this._bodyEl) {
            this._bodyEl.replaceChildren(...this._buildContent());
        }
    },

    /**
     * Close the modal (delegates to Modal.close).
     */
    close() {
        Modal.close(false);
    },

    // ── Skeleton ───────────────────────────────────────────────────────────────

    /**
     * @returns {HTMLElement}
     */
    _buildSkeleton() {
        const body = document.createElement('div');
        body.className = 'smd-body';
        const skel = document.createElement('div');
        skel.className = 'smd-skeleton';
        for (const cls of ['smd-skeleton-line smd-skeleton-line--short', 'smd-skeleton-line smd-skeleton-line--medium', 'smd-skeleton-line']) {
            const line = document.createElement('div');
            line.className = cls;
            skel.appendChild(line);
        }
        body.appendChild(skel);
        return body;
    },

    // ── Content builder ────────────────────────────────────────────────────────

    /**
     * Build the two sections (People + Links) as an array of elements.
     * @returns {HTMLElement[]}
     */
    _buildContent() {
        return [this._buildPeopleSection(), this._buildLinksSection()];
    },

    // ── People section ─────────────────────────────────────────────────────────

    /**
     * @returns {HTMLElement}
     */
    _buildPeopleSection() {
        const section = document.createElement('div');
        section.className = 'smd-section';

        const title = document.createElement('div');
        title.className = 'smd-section-title';
        title.textContent = i18n.t('share.people', 'People');
        section.appendChild(title);

        if (addressBook.isSystemAvailable()) {
            section.appendChild(this._buildSearchRow());
            section.appendChild(this._buildChipsRow());
        } else {
            const note = document.createElement('p');
            note.className = 'smd-directory-unavailable';
            note.textContent = i18n.t('share.directoryUnavailable', 'User directory unavailable');
            section.appendChild(note);
        }

        section.appendChild(this._buildMemberGroups());
        return section;
    },

    /**
     * @returns {HTMLElement}
     */
    _buildSearchRow() {
        const row = document.createElement('div');
        row.className = 'smd-search-row';

        // ── Search input + dropdown ──────────────────────────────────────────
        const wrap = document.createElement('div');
        wrap.className = 'smd-search-wrap';

        const input = document.createElement('input');
        input.type = 'text';
        input.className = 'smd-search-input';
        input.placeholder = i18n.t('share.searchPlaceholder', 'Search people…');
        input.autocomplete = 'off';

        const dropdown = document.createElement('div');
        dropdown.className = 'smd-suggestions hidden';

        wrap.appendChild(input);
        wrap.appendChild(dropdown);

        // ── Role select ──────────────────────────────────────────────────────
        const roleSelect = document.createElement('select');
        roleSelect.className = 'smd-role-select';
        for (const [val, label] of [
            ['viewer', i18n.t('share.role.viewer', 'Viewer')],
            ['editor', i18n.t('share.role.editor', 'Editor')],
            ['admin', i18n.t('share.role.admin', 'Admin')]
        ]) {
            const opt = document.createElement('option');
            opt.value = val;
            opt.textContent = label;
            if (val === this._stagedRole) opt.selected = true;
            roleSelect.appendChild(opt);
        }
        roleSelect.addEventListener('change', () => {
            this._stagedRole = /** @type {ShareRoleEnum} */ (roleSelect.value);
        });

        // ── Add button ───────────────────────────────────────────────────────
        const addBtn = document.createElement('button');
        addBtn.className = 'smd-add-btn btn btn-secondary';
        addBtn.textContent = i18n.t('actions.add', 'Add');
        addBtn.disabled = true;

        // Search debounce
        /** @type {ReturnType<typeof setTimeout>|null} */
        let debounce = null;

        input.addEventListener('input', () => {
            if (debounce) clearTimeout(debounce);
            const q = input.value.trim();
            if (!q) {
                dropdown.classList.add('hidden');
                dropdown.replaceChildren();
                return;
            }
            debounce = setTimeout(async () => {
                const results = await addressBook.searchContacts(q, [SYSTEM_BOOK_ID]);
                this._renderSuggestions(dropdown, results.slice(0, 8), (contact) => {
                    this._stageUser(contact, input, dropdown, addBtn);
                });
            }, 200);
        });

        // Close dropdown on click outside
        document.addEventListener(
            'click',
            (e) => {
                if (!wrap.contains(/** @type {Node} */ (e.target))) {
                    dropdown.classList.add('hidden');
                }
            },
            { once: false }
        );

        addBtn.addEventListener('click', () => {
            if (this._stagedUsers.length === 0) return;
            this._commitStagedUsers();
            addBtn.disabled = true;
        });

        row.appendChild(wrap);
        row.appendChild(roleSelect);
        row.appendChild(addBtn);

        return row;
    },

    /**
     * @param {HTMLElement}                container
     * @param {ContactItem[]}              results
     * @param {(c: ContactItem) => void}   onSelect
     */
    _renderSuggestions(container, results, onSelect) {
        container.replaceChildren();
        if (results.length === 0) {
            container.classList.add('hidden');
            return;
        }
        results.forEach((c, i) => {
            const item = document.createElement('div');
            item.className = 'smd-suggestion-item';
            item.tabIndex = 0;

            const avatar = document.createElement('div');
            avatar.className = `smd-suggestion-avatar smd-avatar--${i % 5}`;
            const displayName = [c.first_name, c.last_name].filter(Boolean).join(' ') || c.full_name || c.id.slice(0, 8);
            avatar.textContent = _initials(displayName);

            const nameEl = document.createElement('span');
            nameEl.className = 'smd-suggestion-name';
            nameEl.textContent = displayName;

            const primaryEmail = c.email?.find((e) => e.is_primary)?.email ?? c.email?.[0]?.email ?? '';
            if (primaryEmail) {
                const emailEl = document.createElement('span');
                emailEl.className = 'smd-suggestion-email';
                emailEl.textContent = primaryEmail;
                item.appendChild(avatar);
                item.appendChild(nameEl);
                item.appendChild(emailEl);
            } else {
                item.appendChild(avatar);
                item.appendChild(nameEl);
            }

            const select = () => onSelect(c);
            item.addEventListener('click', select);
            item.addEventListener('keydown', (e) => {
                if (e.key === 'Enter') select();
            });
            container.appendChild(item);
        });
        container.classList.remove('hidden');
    },

    /**
     * @param {ContactItem}     contact
     * @param {HTMLInputElement} inputEl
     * @param {HTMLElement}      dropdown
     * @param {HTMLButtonElement} addBtn
     */
    _stageUser(contact, inputEl, dropdown, addBtn) {
        // Idempotent: skip duplicates and already-existing members
        const alreadyMember = this._localMembers.some((m) => m.grant.subject.id === contact.id && m._op !== 'remove');
        const alreadyStaged = this._stagedUsers.some((u) => u.id === contact.id);
        if (alreadyMember || alreadyStaged) return;

        this._stagedUsers.push(contact);
        this._refreshChips();
        addBtn.disabled = false;

        inputEl.value = '';
        dropdown.classList.add('hidden');
        dropdown.replaceChildren();
    },

    /**
     * @returns {HTMLElement}
     */
    _buildChipsRow() {
        const row = document.createElement('div');
        row.id = 'smd-chips-row';
        row.className = 'smd-chips';
        this._renderChipsInto(row);
        return row;
    },

    _refreshChips() {
        const row = /** @type {HTMLElement|null} */ (document.getElementById('smd-chips-row'));
        if (row) this._renderChipsInto(row);
    },

    /**
     * @param {HTMLElement} container
     */
    _renderChipsInto(container) {
        container.replaceChildren();
        this._stagedUsers.forEach((c, i) => {
            const chip = document.createElement('div');
            chip.className = 'smd-chip';

            const avatar = document.createElement('div');
            avatar.className = `smd-chip-avatar smd-avatar--${i % 5}`;
            const displayName = [c.first_name, c.last_name].filter(Boolean).join(' ') || c.full_name || c.id.slice(0, 8);
            avatar.textContent = _initials(displayName);

            const nameEl = document.createElement('span');
            nameEl.textContent = displayName;

            const rm = document.createElement('button');
            rm.className = 'smd-chip-remove';
            rm.innerHTML = '&times;';
            rm.title = i18n.t('actions.remove', 'Remove');
            rm.addEventListener('click', () => {
                this._stagedUsers = this._stagedUsers.filter((u) => u.id !== c.id);
                this._refreshChips();
                const addBtn = /** @type {HTMLButtonElement|null} */ (document.querySelector('.smd-add-btn'));
                if (addBtn) addBtn.disabled = this._stagedUsers.length === 0;
            });

            chip.appendChild(avatar);
            chip.appendChild(nameEl);
            chip.appendChild(rm);
            container.appendChild(chip);
        });
    },

    _commitStagedUsers() {
        for (const contact of this._stagedUsers) {
            this._localMembers.push({
                grant: {
                    id: '', // not yet persisted
                    granted_at: 0, // placeholder — grant hasn't been persisted yet
                    granted_by: '',
                    subject: { type: 'user', id: contact.id },
                    permission: /** @type {import('../core/types.js').PermissionTypeEnum} */ (ROLE_PERMISSIONS[this._stagedRole][0]),
                    resource: { type: this._itemType, id: this._item?.id ?? '' }
                },
                role: this._stagedRole,
                _op: 'new'
            });
        }
        this._stagedUsers = [];
        this._refreshChips();
        this._refreshMemberGroups();
    },

    /**
     * @returns {HTMLElement}
     */
    _buildMemberGroups() {
        const container = document.createElement('div');
        container.id = 'smd-member-groups';
        this._renderMemberGroupsInto(container);
        return container;
    },

    _refreshMemberGroups() {
        const container = /** @type {HTMLElement|null} */ (document.getElementById('smd-member-groups'));
        if (container) this._renderMemberGroupsInto(container);
    },

    /**
     * @param {HTMLElement} container
     */
    _renderMemberGroupsInto(container) {
        container.replaceChildren();
        const groups = /** @type {ShareRoleEnum[]} */ (['admin', 'editor', 'viewer']);
        let memberIndex = 0;

        for (const role of groups) {
            const visible = this._localMembers.filter((m) => m.role === role && m._op !== 'remove');
            if (visible.length === 0) continue;

            const group = document.createElement('div');
            group.className = 'smd-group';

            const header = document.createElement('div');
            header.className = 'smd-group-header';

            const labelMap = {
                admin: i18n.t('share.role.admin', 'Admin'),
                editor: i18n.t('share.role.editor', 'Editor'),
                viewer: i18n.t('share.role.viewer', 'Viewer')
            };
            const badge = document.createElement('span');
            badge.className = 'smd-group-badge';
            badge.textContent = String(visible.length);
            header.textContent = labelMap[role];
            header.appendChild(badge);
            group.appendChild(header);

            for (const entry of visible) {
                group.appendChild(this._buildMemberRow(entry, memberIndex));
                memberIndex++;
            }
            container.appendChild(group);
        }
    },

    /**
     * @param {MemberEntry} entry
     * @param {number}      idx
     * @returns {HTMLElement}
     */
    _buildMemberRow(entry, idx) {
        const row = document.createElement('div');
        row.className = 'smd-member-row';

        const avatar = document.createElement('div');
        avatar.className = `smd-member-avatar smd-avatar--${idx % 5}`;

        // Resolve display name async
        systemUsers.getDisplayName(entry.grant.subject.id).then((name) => {
            avatar.textContent = _initials(name);
            nameEl.textContent = name;
        });

        const nameEl = document.createElement('span');
        nameEl.className = 'smd-member-name';
        nameEl.textContent = `${entry.grant.subject.id.slice(0, 8)}…`;

        const roleSelect = document.createElement('select');
        roleSelect.className = 'smd-member-role-select';
        for (const [val, label] of [
            ['viewer', i18n.t('share.role.viewer', 'Viewer')],
            ['editor', i18n.t('share.role.editor', 'Editor')],
            ['admin', i18n.t('share.role.admin', 'Admin')]
        ]) {
            const opt = document.createElement('option');
            opt.value = val;
            opt.textContent = label;
            if (val === entry.role) opt.selected = true;
            roleSelect.appendChild(opt);
        }
        roleSelect.addEventListener('change', () => {
            const newRole = /** @type {ShareRoleEnum} */ (roleSelect.value);
            entry.role = newRole;
            entry._op = entry._op === 'new' ? 'new' : 'change';
            this._refreshMemberGroups();
        });

        const removeBtn = document.createElement('button');
        removeBtn.className = 'smd-row-action';
        removeBtn.title = i18n.t('actions.remove', 'Remove');
        removeBtn.innerHTML = '<i class="fas fa-times"></i>';
        removeBtn.addEventListener('click', () => {
            entry._op = 'remove';
            this._refreshMemberGroups();
        });

        row.appendChild(avatar);
        row.appendChild(nameEl);
        row.appendChild(roleSelect);
        row.appendChild(removeBtn);
        return row;
    },

    // ── Links section ──────────────────────────────────────────────────────────

    /**
     * @returns {HTMLElement}
     */
    _buildLinksSection() {
        const section = document.createElement('div');
        section.className = 'smd-section';

        const title = document.createElement('div');
        title.className = 'smd-section-title';
        title.textContent = i18n.t('share.publicLinks', 'Public links');
        section.appendChild(title);

        const listEl = document.createElement('div');
        listEl.id = 'smd-links-list';
        this._renderLinksInto(listEl);
        section.appendChild(listEl);

        const newLinkBtn = document.createElement('button');
        newLinkBtn.className = 'smd-new-link-btn';
        newLinkBtn.innerHTML = `<i class="fas fa-plus"></i> ${i18n.t('share.createLink', 'Create new public link')}`;
        newLinkBtn.id = 'smd-new-link-btn';

        const newLinkForm = document.createElement('div');
        newLinkForm.id = 'smd-new-link-form';
        newLinkForm.className = 'smd-new-link-form hidden';
        newLinkForm.appendChild(this._buildNewLinkForm(newLinkBtn, newLinkForm));

        newLinkBtn.addEventListener('click', () => {
            newLinkBtn.classList.add('hidden');
            newLinkForm.classList.remove('hidden');
        });

        section.appendChild(newLinkBtn);
        section.appendChild(newLinkForm);
        return section;
    },

    /**
     * @param {HTMLElement} container
     */
    _renderLinksInto(container) {
        container.replaceChildren();

        // Existing links
        for (const entry of this._localLinks.filter((e) => e._op !== 'remove')) {
            container.appendChild(this._buildLinkRow(entry));
        }

        // Draft (new) links
        for (const draft of this._newLinks) {
            container.appendChild(this._buildDraftLinkRow(draft));
        }
    },

    _refreshLinks() {
        const container = /** @type {HTMLElement|null} */ (document.getElementById('smd-links-list'));
        if (container) this._renderLinksInto(container);
    },

    /**
     * @param {LinkEntry} entry
     * @returns {HTMLElement}
     */
    _buildLinkRow(entry) {
        const share = entry.share;
        const draft = entry._op === 'edit' ? entry._draft : null;

        // Display values: prefer draft overrides when in edit-pending state
        const displayName = draft?.name ? draft.name : share.item_name || i18n.t('share.sharedLink', 'Shared link');
        const displayPw = draft ? draft.password !== null : share.has_password;
        const displayExp = draft ? draft.expires_at : share.expires_at ? fileSharing.formatExpirationDate(share.expires_at) : null;

        const row = document.createElement('div');
        row.className = 'smd-link-row';

        const icon = document.createElement('div');
        icon.className = 'smd-link-icon';
        icon.innerHTML = '<i class="fas fa-link"></i>';

        const info = document.createElement('div');
        info.className = 'smd-link-info';

        const name = document.createElement('div');
        name.className = 'smd-link-name';
        name.textContent = displayName;

        const tags = document.createElement('div');
        tags.className = 'smd-link-tags';
        if (displayPw) {
            const t = document.createElement('span');
            t.className = 'smd-link-tag';
            t.innerHTML = `<i class="fas fa-lock"></i> ${i18n.t('share.passwordProtected', 'Password')}`;
            tags.appendChild(t);
        }
        if (displayExp) {
            const t = document.createElement('span');
            t.className = 'smd-link-tag';
            t.innerHTML = `<i class="fas fa-clock"></i> ${displayExp}`;
            tags.appendChild(t);
        }

        info.appendChild(name);
        if (tags.children.length) info.appendChild(tags);

        const actions = document.createElement('div');
        actions.className = 'smd-link-actions';

        // Copy
        const copyBtn = document.createElement('button');
        copyBtn.className = 'smd-row-action';
        copyBtn.title = i18n.t('actions.copy', 'Copy');
        copyBtn.innerHTML = '<i class="fas fa-copy"></i>';
        copyBtn.addEventListener('click', () => fileSharing.copyLinkToClipboard(share.url));

        // Edit
        const editBtn = document.createElement('button');
        editBtn.className = 'smd-row-action';
        editBtn.title = i18n.t('actions.edit', 'Edit');
        editBtn.innerHTML = '<i class="fas fa-pencil-alt"></i>';
        editBtn.addEventListener('click', () => {
            const panel = row.nextElementSibling;
            if (panel?.classList.contains('smd-edit-panel')) {
                panel.classList.toggle('hidden');
            } else {
                const editPanel = this._buildEditPanel(entry, row);
                row.after(editPanel);
            }
        });

        // Delete
        const delBtn = document.createElement('button');
        delBtn.className = 'smd-row-action';
        delBtn.title = i18n.t('actions.delete', 'Delete');
        delBtn.innerHTML = '<i class="fas fa-trash-alt"></i>';
        delBtn.addEventListener('click', () => {
            entry._op = 'remove';
            this._refreshLinks();
        });

        actions.appendChild(copyBtn);
        actions.appendChild(editBtn);
        actions.appendChild(delBtn);

        row.appendChild(icon);
        row.appendChild(info);
        row.appendChild(actions);
        return row;
    },

    /**
     * @param {DraftLink} draft
     * @returns {HTMLElement}
     */
    _buildDraftLinkRow(draft) {
        const row = document.createElement('div');
        row.className = 'smd-link-row';

        const icon = document.createElement('div');
        icon.className = 'smd-link-icon';
        icon.innerHTML = '<i class="fas fa-link"></i>';

        const info = document.createElement('div');
        info.className = 'smd-link-info';

        const name = document.createElement('div');
        name.className = 'smd-link-name';
        name.textContent = draft.name || i18n.t('share.newLink', 'New link');

        const tags = document.createElement('div');
        tags.className = 'smd-link-tags';
        if (draft.password) {
            const t = document.createElement('span');
            t.className = 'smd-link-tag';
            t.innerHTML = `<i class="fas fa-lock"></i> ${i18n.t('share.passwordProtected', 'Password')}`;
            tags.appendChild(t);
        }
        if (draft.expires_at) {
            const t = document.createElement('span');
            t.className = 'smd-link-tag';
            t.innerHTML = `<i class="fas fa-clock"></i> ${draft.expires_at}`;
            tags.appendChild(t);
        }

        const pending = document.createElement('span');
        pending.className = 'smd-link-tag';
        pending.textContent = i18n.t('share.pending', 'Pending');
        tags.appendChild(pending);

        info.appendChild(name);
        if (tags.children.length) info.appendChild(tags);

        const actions = document.createElement('div');
        actions.className = 'smd-link-actions';

        const delBtn = document.createElement('button');
        delBtn.className = 'smd-row-action';
        delBtn.title = i18n.t('actions.remove', 'Remove');
        delBtn.innerHTML = '<i class="fas fa-times"></i>';
        delBtn.addEventListener('click', () => {
            this._newLinks = this._newLinks.filter((d) => d !== draft);
            this._refreshLinks();
        });

        actions.appendChild(delBtn);
        row.appendChild(icon);
        row.appendChild(info);
        row.appendChild(actions);
        return row;
    },

    /**
     * @param {LinkEntry} entry
     * @param {HTMLElement} row
     * @returns {HTMLElement}
     */
    _buildEditPanel(entry, row) {
        const panel = document.createElement('div');
        panel.className = 'smd-edit-panel';

        const pwLabel = document.createElement('label');
        pwLabel.textContent = i18n.t('dialogs.password', 'Password');
        const pwInput = document.createElement('input');
        pwInput.type = 'password';
        pwInput.className = 'smd-edit-input';
        pwInput.placeholder = i18n.t('share.passwordPlaceholder', 'Leave empty to keep unchanged');

        const expLabel = document.createElement('label');
        expLabel.textContent = i18n.t('dialogs.expiration', 'Expiration date');
        const expInput = document.createElement('input');
        expInput.type = 'date';
        expInput.className = 'smd-edit-input';
        if (entry.share.expires_at) {
            expInput.value = new Date(entry.share.expires_at * 1000).toISOString().slice(0, 10);
        }

        const actionsDiv = document.createElement('div');
        actionsDiv.className = 'smd-edit-panel-actions';

        const cancelBtn = document.createElement('button');
        cancelBtn.className = 'btn btn-secondary';
        cancelBtn.textContent = i18n.t('actions.cancel', 'Cancel');
        cancelBtn.addEventListener('click', () => panel.remove());

        const saveBtn = document.createElement('button');
        saveBtn.className = 'btn btn-primary';
        saveBtn.textContent = i18n.t('actions.save', 'Save');
        saveBtn.addEventListener('click', () => {
            entry._op = 'edit';
            entry._draft = {
                name: entry.share.item_name || '',
                password: pwInput.value || null,
                expires_at: expInput.value || null
            };
            panel.remove();
            this._refreshLinks();
        });

        actionsDiv.appendChild(cancelBtn);
        actionsDiv.appendChild(saveBtn);

        panel.appendChild(pwLabel);
        panel.appendChild(pwInput);
        panel.appendChild(expLabel);
        panel.appendChild(expInput);
        panel.appendChild(actionsDiv);

        void row; // row is unused — panel is inserted via row.after() in caller
        return panel;
    },

    /**
     * @param {HTMLButtonElement} newLinkBtn
     * @param {HTMLElement}       formWrapper
     * @returns {HTMLElement}
     */
    _buildNewLinkForm(newLinkBtn, formWrapper) {
        const inner = document.createElement('div');

        const nameLabel = document.createElement('label');
        nameLabel.textContent = i18n.t('share.linkName', 'Link name');
        const nameInput = document.createElement('input');
        nameInput.type = 'text';
        nameInput.className = 'smd-edit-input';
        nameInput.placeholder = i18n.t('share.linkNamePlaceholder', 'Optional name');

        const pwToggleLabel = document.createElement('label');
        pwToggleLabel.className = 'smd-pw-toggle';
        const pwCheckbox = document.createElement('input');
        pwCheckbox.type = 'checkbox';
        pwToggleLabel.appendChild(pwCheckbox);
        pwToggleLabel.appendChild(document.createTextNode(` ${i18n.t('share.addPassword', 'Add password')}`));

        const pwInput = document.createElement('input');
        pwInput.type = 'password';
        pwInput.className = 'smd-edit-input hidden';
        pwInput.placeholder = i18n.t('dialogs.password', 'Password');
        pwCheckbox.addEventListener('change', () => {
            pwInput.classList.toggle('hidden', !pwCheckbox.checked);
        });

        const expLabel = document.createElement('label');
        expLabel.textContent = i18n.t('dialogs.expiration', 'Expiration date');
        const expInput = document.createElement('input');
        expInput.type = 'date';
        expInput.className = 'smd-edit-input';

        const actionsDiv = document.createElement('div');
        actionsDiv.className = 'smd-new-link-form-actions';

        const cancelBtn = document.createElement('button');
        cancelBtn.className = 'btn btn-secondary';
        cancelBtn.textContent = i18n.t('actions.cancel', 'Cancel');
        cancelBtn.addEventListener('click', () => {
            formWrapper.classList.add('hidden');
            newLinkBtn.classList.remove('hidden');
        });

        const addBtn = document.createElement('button');
        addBtn.className = 'btn btn-primary';
        addBtn.textContent = i18n.t('share.addLink', 'Add link');
        addBtn.addEventListener('click', () => {
            /** @type {DraftLink} */
            const draft = {
                name: nameInput.value.trim(),
                password: pwCheckbox.checked ? pwInput.value || null : null,
                expires_at: expInput.value || null
            };
            this._newLinks.push(draft);
            this._refreshLinks();

            // Reset form
            nameInput.value = '';
            pwCheckbox.checked = false;
            pwInput.value = '';
            pwInput.classList.add('hidden');
            expInput.value = '';

            formWrapper.classList.add('hidden');
            newLinkBtn.classList.remove('hidden');
        });

        actionsDiv.appendChild(cancelBtn);
        actionsDiv.appendChild(addBtn);

        inner.appendChild(nameLabel);
        inner.appendChild(nameInput);
        inner.appendChild(pwToggleLabel);
        inner.appendChild(pwInput);
        inner.appendChild(expLabel);
        inner.appendChild(expInput);
        inner.appendChild(actionsDiv);

        return inner;
    },

    // ── Apply ──────────────────────────────────────────────────────────────────

    /**
     * Commit all pending local operations to the server, then close.
     * @returns {Promise<void>}
     */
    async _applyAll() {
        if (!this._item) return;

        // Disable the Apply button while working
        if (Modal.confirmBtn) Modal.confirmBtn.disabled = true;

        const item = this._item;
        const itemType = this._itemType;

        try {
            // ── Grants ─────────────────────────────────────────────────────────
            for (const m of this._localMembers) {
                if (m._op === 'remove' && m.grant.id) {
                    await grants.revokeGrant(m.grant.id);
                } else if (m._op === 'change' && m.grant.id) {
                    await grants.updateRole({
                        subject: { type: m.grant.subject.type, id: m.grant.subject.id },
                        resource: { type: itemType, id: item.id },
                        role: m.role
                    });
                } else if (m._op === 'new') {
                    await grants.createGrant({
                        subject: { type: m.grant.subject.type, id: m.grant.subject.id },
                        resource: { type: itemType, id: item.id },
                        role: m.role
                    });
                }
            }

            // ── Links ──────────────────────────────────────────────────────────
            for (const e of this._localLinks) {
                if (e._op === 'remove') {
                    await fileSharing.removeSharedLink(e.share.id);
                } else if (e._op === 'edit' && e._draft) {
                    const expiresTs = e._draft.expires_at ? Math.floor(new Date(e._draft.expires_at).getTime() / 1000) : null;
                    await fileSharing.updateSharedLink(e.share.id, {
                        password: e._draft.password,
                        expires_at: expiresTs,
                        permissions: null
                    });
                }
            }

            for (const draft of this._newLinks) {
                await fileSharing.createSharedLink(
                    item.id,
                    itemType,
                    /** @type {import('../core/types.js').CreateShare} */ ({
                        item_id: item.id,
                        item_name: item.name ?? null,
                        item_type: itemType,
                        password: draft.password,
                        // Pass as ms timestamp so fileSharing's new Date(expires_at) works correctly
                        expires_at: draft.expires_at ? new Date(draft.expires_at).getTime() : null,
                        permissions: { read: true, write: false, reshare: false }
                    })
                );
            }

            // ── Refresh badge cache ────────────────────────────────────────────
            await grants.fetchOutgoingGrants();

            const hasAnyShare =
                this._localMembers.some((m) => m._op !== 'remove') || this._localLinks.some((e) => e._op !== 'remove') || this._newLinks.length > 0;

            ui.setSharedVisualState(item.id, itemType, hasAnyShare);

            Modal.close(true);
        } catch (err) {
            console.error('shareModal._applyAll error:', err);
            if (Modal.confirmBtn) Modal.confirmBtn.disabled = false;
        }
    }
};

export { shareModal };
