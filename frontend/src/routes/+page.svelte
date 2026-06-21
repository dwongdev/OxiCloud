<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';
	import { session } from '$lib/stores/session.svelte';

	// The app root redirects by user kind:
	//   - external users (magic-link / OIDC-only / OCM recipients) have no
	//     personal drive, so they land on Shared-with-me;
	//   - internal users go to the files browser, which resolves the default
	//     personal drive's root folder via `session.loadHomeFolder()` (post-D0
	//     this reads `GET /api/drives` and picks the row whose
	//     `default_for_user` matches the caller).
	onMount(() => {
		const target = session.isExternalUser ? '/shared-with-me' : '/files';
		void goto(resolve(target), { replaceState: true });
	});
</script>

<p>Loading…</p>
