<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import type { Pathname } from '$app/types';
	import { page, updated } from '$app/state';
	import { onMount } from 'svelte';
	import '$lib/styles/app.css';
	import AppShell from '$lib/components/AppShell.svelte';
	import DialogHost from '$lib/components/DialogHost.svelte';
	import Toaster from '$lib/components/Toaster.svelte';
	import { session } from '$lib/stores/session.svelte';
	import { ui } from '$lib/stores/ui.svelte';
	import { hashUrlToPath } from '$lib/utils/hashRedirect';

	let { children } = $props();

	// A new build was deployed (`_app/version.json` changed, polled per
	// svelte.config.js) — reload so an open tab never keeps running stale code
	// after a rebuild. Deferred while a progress notification (e.g. an upload) is
	// in flight so we don't interrupt it; this effect re-runs when that clears
	// (notifications are reactive) and reloads then.
	$effect(() => {
		if (!updated.current) return;
		const busy = ui.notifications.some((n) => n.progress !== undefined);
		if (!busy) location.reload();
	});

	// Routes reachable without an authenticated session.
	const PUBLIC_PREFIXES = ['/login', '/device', '/s/', '/nextcloud'];

	function isPublic(pathname: string): boolean {
		return PUBLIC_PREFIXES.some((p) => pathname === p || pathname.startsWith(p));
	}

	let ready = $state(false);

	onMount(async () => {
		// The instant HTML boot splash has done its job — the app is mounted, so
		// the route (login renders immediately; protected routes show their own
		// loading state) is already in the DOM behind it.
		document.getElementById('app-splash')?.remove();

		// Redirect old `#/...` bookmarks to the new path before anything else.
		if (typeof location !== 'undefined' && location.hash.startsWith('#/')) {
			// hashUrlToPath returns a dynamic in-app path string; resolve() is typed
			// for known route ids, so assert it as a Pathname (same precedent as the
			// post-login redirect target).
			const mapped = hashUrlToPath(location.hash);
			if (mapped) await goto(resolve(mapped as Pathname), { replaceState: true });
		}
		await session.load();
		ready = true;
	});

	// Guard: once the session is known, bounce unauthenticated users off
	// protected routes. Runs client-side only (ssr=false).
	$effect(() => {
		if (!ready) return;
		const path = page.url.pathname;
		if (!session.isAuthenticated && !isPublic(path)) {
			void goto(resolve(`/login?redirect=${encodeURIComponent(path)}`), { replaceState: true });
		}
	});
</script>

{#if isPublic(page.url.pathname)}
	{@render children()}
{:else if ready && session.isAuthenticated}
	<AppShell {children} />
{:else if ready}
	{@render children()}
{:else}
	<div class="app-loading" aria-busy="true">Loading…</div>
{/if}

<Toaster />
<DialogHost />

<style>
	.app-loading {
		display: grid;
		place-items: center;
		min-height: 100vh;
		color: var(--color-text-muted);
	}
</style>
