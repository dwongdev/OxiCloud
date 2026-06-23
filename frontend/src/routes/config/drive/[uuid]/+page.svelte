<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { onMount } from 'svelte';

	import {
		listDriveMembers,
		removeDriveMember,
		updateDriveMember
	} from '$lib/api/endpoints/drives';
	import type { Drive, DriveMember, DriveRole } from '$lib/api/types';
	import UserVignette from '$lib/components/UserVignette.svelte';
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n/index.svelte';
	import { drives as drivesStore, driveIcon } from '$lib/stores/drives.svelte';
	import { errorToast } from '$lib/utils/errors';
	import { formatDate } from '$lib/utils/display';
	import { formatBytes } from '$lib/utils/format';

	const uuid = $derived(page.params.uuid ?? '');
	const drive = $derived<Drive | null>(drivesStore.findById(uuid));

	let members = $state<DriveMember[]>([]);
	let membersLoaded = $state(false);
	let membersError = $state<string | null>(null);

	// Mutation controls are gated by *both* caller_role AND drive kind:
	// even an Owner of a personal drive can't change membership (the
	// backend guard refuses), so the UI hides the controls upfront for
	// honest UX. Shared drives + Owner role → full controls.
	const canManageMembers = $derived(drive?.kind === 'shared' && drive?.caller_role === 'owner');

	// Roles offered in the dropdown. Owner sets the bundle; other roles
	// match the backend `Role` enum order (owner → viewer = strongest → weakest).
	const ASSIGNABLE_ROLES: DriveRole[] = ['owner', 'editor', 'viewer'];

	function roleLabel(role: DriveRole): string {
		switch (role) {
			case 'owner':
				return t('drive.role.owner', 'Owner');
			case 'editor':
				return t('drive.role.editor', 'Editor');
			case 'contributor':
				return t('drive.role.contributor', 'Contributor');
			case 'commenter':
				return t('drive.role.commenter', 'Commenter');
			case 'viewer':
				return t('drive.role.viewer', 'Viewer');
		}
	}

	async function loadMembers() {
		if (!uuid) return;
		try {
			members = await listDriveMembers(uuid);
		} catch (e) {
			// 404 here means the caller lacks Read on the drive — which is
			// also what the parent "Drive not found" card already conveys.
			// Keep the listing area empty rather than surfacing a noisy toast.
			membersError = e instanceof Error ? e.message : String(e);
			members = [];
		} finally {
			membersLoaded = true;
		}
	}

	async function changeRole(member: DriveMember, role: DriveRole) {
		if (member.role === role) return;
		try {
			const updated = await updateDriveMember(uuid, member.subject, role);
			members = members.map((m) => (m.id === member.id ? updated : m));
		} catch (e) {
			errorToast(e);
			// Re-fetch so the dropdown reflects the server-side state, not the
			// optimistic-but-rejected change.
			await loadMembers();
		}
	}

	async function removeMember(member: DriveMember) {
		try {
			await removeDriveMember(uuid, member.subject);
			members = members.filter((m) => m.id !== member.id);
		} catch (e) {
			errorToast(e);
			await loadMembers();
		}
	}

	const kindLabel = $derived.by(() => {
		if (!drive) return '';
		return drive.kind === 'shared'
			? t('drive.kind_shared', 'Shared drive')
			: t('drive.kind_personal', 'Personal drive');
	});

	const storagePct = $derived.by(() => {
		if (!drive || !drive.quota_bytes || drive.quota_bytes <= 0) return 0;
		return Math.min(100, (drive.used_bytes / drive.quota_bytes) * 100);
	});

	const policyEntries = $derived.by(() => {
		if (!drive) return [];
		return Object.entries(drive.policies).map(([key, value]) => ({ key, value }));
	});

	function policyLabel(key: string): string {
		// Known policy keys get a friendlier translated label; unknown keys
		// surface verbatim so operators still see them (forward-compat).
		switch (key) {
			case 'forbid_public_links':
				return t('drive.policy.forbid_public_links', 'Forbid public links');
			case 'forbid_external_sharing':
				return t('drive.policy.forbid_external_sharing', 'Forbid external sharing');
			case 'forbid_sharing':
				return t('drive.policy.forbid_sharing', 'Forbid sharing');
			case 'forbid_cross_drive_move':
				return t('drive.policy.forbid_cross_drive_move', 'Forbid cross-drive move');
			case 'include_in_photo_index':
				return t('drive.policy.include_in_photo_index', 'Include in photo index');
			case 'forbid_music_index':
				return t('drive.policy.forbid_music_index', 'Forbid music index');
			default:
				return key;
		}
	}

	function policyValueDisplay(value: unknown): string {
		if (value === true) return t('drive.policy.on', 'On');
		if (value === false) return t('drive.policy.off', 'Off');
		return String(value);
	}

	onMount(() => {
		void drivesStore.load();
		void loadMembers();
	});
</script>

<div class="config-drive">
	{#if !drivesStore.loaded}
		<p class="muted">{t('common.loading', 'Loading…')}</p>
	{:else if !drive}
		<div class="card">
			<h2>{t('drive.not_found_title', 'Drive not found')}</h2>
			<p class="muted">
				{t('drive.not_found_body', "This drive doesn't exist or you don't have access to it.")}
			</p>
			<a class="link" href={resolve('/files')}>{t('drive.back_to_files', 'Back to Files')}</a>
		</div>
	{:else}
		<h1>
			<Icon name={driveIcon(drive)} />
			{drive.name}
		</h1>

		<div class="card">
			<h2><Icon name="info-circle" /> {t('drive.info', 'Drive info')}</h2>
			<dl class="info-grid">
				<dt>{t('drive.field.kind', 'Kind')}</dt>
				<dd>{kindLabel}</dd>

				{#if drive.default_for_user}
					<dt>{t('drive.field.default', 'Default')}</dt>
					<dd>{t('drive.field.default_yes', 'This is your home drive')}</dd>
				{/if}

				<dt>{t('drive.field.created', 'Created')}</dt>
				<dd>{formatDate(drive.created_at)}</dd>

				<dt>{t('drive.field.updated', 'Last updated')}</dt>
				<dd>{formatDate(drive.updated_at)}</dd>

				<dt>{t('drive.field.id', 'Identifier')}</dt>
				<dd class="mono">{drive.id}</dd>
			</dl>
		</div>

		<div class="card">
			<h2><Icon name="hdd" /> {t('drive.storage', 'Storage')}</h2>
			<div class="storage-row">
				<div class="storage-stat">
					<div class="storage-stat__value">{formatBytes(drive.used_bytes)}</div>
					<div class="storage-stat__label">{t('drive.used', 'Used')}</div>
				</div>
				<div class="storage-stat">
					<div class="storage-stat__value">
						{drive.quota_bytes && drive.quota_bytes > 0 ? formatBytes(drive.quota_bytes) : '∞'}
					</div>
					<div class="storage-stat__label">{t('drive.quota', 'Quota')}</div>
				</div>
				<div class="storage-stat">
					<div class="storage-stat__value">
						{drive.quota_bytes && drive.quota_bytes > 0 ? `${Math.round(storagePct)}%` : '—'}
					</div>
					<div class="storage-stat__label">{t('drive.usage', 'Usage')}</div>
				</div>
			</div>
			{#if drive.quota_bytes && drive.quota_bytes > 0}
				<div
					class="bar"
					role="progressbar"
					aria-valuemin="0"
					aria-valuemax="100"
					aria-valuenow={Math.round(storagePct)}
				>
					<div class="bar__fill" style:width="{storagePct}%"></div>
				</div>
			{/if}
		</div>

		<div class="card">
			<h2><Icon name="users" /> {t('drive.members', 'Members')}</h2>
			{#if !membersLoaded}
				<p class="muted">{t('common.loading', 'Loading…')}</p>
			{:else if members.length === 0}
				<p class="muted">
					{membersError ?? t('drive.members_empty', 'No members.')}
				</p>
			{:else}
				<ul class="members">
					{#each members as m (m.id)}
						<li class="members__row">
							{#if m.subject.type === 'user'}
								<UserVignette userId={m.subject.id} />
							{:else if m.subject.type === 'group'}
								<span class="members__group">
									<Icon name="users" />
									<span class="mono">{m.subject.id}</span>
								</span>
							{:else}
								<span class="members__token">
									<Icon name="link" />
									<span class="mono">{m.subject.id}</span>
								</span>
							{/if}

							{#if canManageMembers}
								<select
									class="members__role-select"
									value={m.role}
									onchange={(e) =>
										void changeRole(m, (e.currentTarget as HTMLSelectElement).value as DriveRole)}
									aria-label={t('drive.member.change_role_aria', 'Change role')}
								>
									{#each ASSIGNABLE_ROLES as r (r)}
										<option value={r}>{roleLabel(r)}</option>
									{/each}
								</select>
								<button
									type="button"
									class="members__remove"
									title={t('drive.member.remove', 'Remove member')}
									aria-label={t('drive.member.remove', 'Remove member')}
									onclick={() => void removeMember(m)}
								>
									<Icon name="times" />
								</button>
							{:else}
								<span class="members__role members__role--{m.role}">
									{roleLabel(m.role)}
								</span>
							{/if}
						</li>
					{/each}
				</ul>

				{#if !canManageMembers && drive.kind === 'personal'}
					<p class="muted members__personal-note">
						{t(
							'drive.members.personal_immutable',
							'Personal drives have a fixed single-owner membership.'
						)}
					</p>
				{/if}
			{/if}
		</div>

		{#if policyEntries.length > 0}
			<div class="card">
				<h2><Icon name="shield-alt" /> {t('drive.policies', 'Policies')}</h2>
				<dl class="info-grid">
					{#each policyEntries as p (p.key)}
						<dt>{policyLabel(p.key)}</dt>
						<dd>{policyValueDisplay(p.value)}</dd>
					{/each}
				</dl>
			</div>
		{/if}
	{/if}
</div>

<style>
	.config-drive {
		max-width: 800px;
		margin: 0 auto;
		padding: 1.5rem 1rem;
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
	}

	.config-drive h1 {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		margin: 0;
		font-size: 1.5rem;
		color: var(--color-text-heading);
	}

	.card {
		background: var(--color-bg-surface);
		border: 1px solid var(--color-border-subtle);
		border-radius: var(--radius-md);
		padding: 1.25rem;
	}

	.card h2 {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		margin: 0 0 1rem;
		font-size: 1.05rem;
		color: var(--color-text-heading);
	}

	.info-grid {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.5rem 1.5rem;
		margin: 0;
	}

	.info-grid dt {
		color: var(--color-text-muted);
		font-size: 0.85rem;
	}

	.info-grid dd {
		margin: 0;
		color: var(--color-text);
	}

	.mono {
		font-family: var(--font-mono);
		font-size: 0.85rem;
	}

	.storage-row {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 1rem;
		margin-bottom: 1rem;
	}

	.storage-stat__value {
		font-size: 1.1rem;
		font-weight: var(--weight-semibold);
		color: var(--color-text);
	}

	.storage-stat__label {
		font-size: 0.8rem;
		color: var(--color-text-muted);
	}

	.bar {
		height: 6px;
		background: var(--color-bg-muted);
		border-radius: 3px;
		overflow: hidden;
	}

	.bar__fill {
		height: 100%;
		background: var(--color-accent);
		transition: width 200ms ease;
	}

	.muted {
		color: var(--color-text-muted);
	}

	.link {
		color: var(--color-accent);
		text-decoration: none;
	}

	.link:hover {
		text-decoration: underline;
	}

	/* Members list */
	.members {
		list-style: none;
		padding: 0;
		margin: 0;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.members__row {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.5rem 0.75rem;
		border: 1px solid var(--color-border-faint);
		border-radius: var(--radius-sm);
		background: var(--color-bg-page);
	}

	.members__group,
	.members__token {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		flex: 1;
		min-width: 0;
		color: var(--color-text-secondary);
	}

	.members__role {
		display: inline-flex;
		align-items: center;
		padding: 0.2rem 0.65rem;
		border-radius: var(--radius-pill, 999px);
		font-size: 0.8rem;
		background: var(--color-bg-muted);
		color: var(--color-text-secondary);
		flex: none;
	}

	.members__role--owner {
		background: var(--color-accent-tint, var(--color-bg-muted));
		color: var(--color-accent-text, var(--color-text-secondary));
		font-weight: var(--weight-semibold);
	}

	.members__role--editor,
	.members__role--contributor {
		background: var(--color-accent-ring, var(--color-bg-muted));
		color: var(--color-accent-text, var(--color-text-secondary));
	}

	.members__role-select {
		flex: none;
		padding: 0.25rem 0.5rem;
		border-radius: var(--radius-sm);
		border: 1px solid var(--color-border);
		background: var(--color-bg-input);
		color: var(--color-text);
		font: inherit;
		font-size: 0.85rem;
		cursor: pointer;
	}

	.members__remove {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 28px;
		height: 28px;
		border: none;
		border-radius: var(--radius-sm);
		background: transparent;
		color: var(--color-text-faint);
		cursor: pointer;
	}

	.members__remove:hover {
		background: var(--color-bg-hover);
		color: var(--color-danger-text, var(--color-text));
	}

	.members__personal-note {
		margin-top: 0.75rem;
		font-size: 0.85rem;
	}
</style>
