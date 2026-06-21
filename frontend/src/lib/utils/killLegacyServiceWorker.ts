export async function killLegacyServiceWorker(): Promise<void> {
	if (typeof navigator === 'undefined' || !('serviceWorker' in navigator)) return;

	try {
		const registrations = await navigator.serviceWorker.getRegistrations();
		const legacy = registrations.filter((r) => {
			const url = r.active?.scriptURL ?? r.waiting?.scriptURL ?? r.installing?.scriptURL ?? '';
			return url.endsWith('/sw.js');
		});
		if (legacy.length === 0) return;

		await Promise.all(legacy.map((r) => r.unregister()));

		if ('caches' in window) {
			const keys = await caches.keys();
			await Promise.all(
				keys.filter((k) => k.startsWith('oxicloud-cache')).map((k) => caches.delete(k))
			);
		}

		if (navigator.serviceWorker.controller && !sessionStorage.getItem('legacy-sw-killed')) {
			sessionStorage.setItem('legacy-sw-killed', '1');
			location.reload();
		}
	} catch {
		/* best-effort cleanup */
	}
}
