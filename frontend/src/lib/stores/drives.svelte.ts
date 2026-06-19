/**
 * Drives store — caches `GET /api/drives` so the picker, the breadcrumb
 * icon, and the session bootstrap all share one fetch. Idempotent `load()`.
 *
 * Identifying the user's home: always via `default_for_user`, never by
 * folder name (users can rename "Personal").
 */
import { listDrives } from '$lib/api/endpoints/drives';
import type { Drive } from '$lib/api/types';

class DrivesStore {
	drives = $state<Drive[]>([]);
	loaded = $state(false);
	private inflight: Promise<Drive[]> | null = null;

	async load(): Promise<Drive[]> {
		if (this.loaded) return this.drives;
		if (this.inflight) return this.inflight;
		this.inflight = (async () => {
			try {
				this.drives = await listDrives();
			} catch {
				this.drives = [];
			} finally {
				this.loaded = true;
				this.inflight = null;
			}
			return this.drives;
		})();
		return this.inflight;
	}

	/** Force a refresh after a mutation (rename, member change, …). */
	invalidate(): void {
		this.loaded = false;
		this.drives = [];
	}

	/** Caller's default-personal drive (one per internal user), or null. */
	findDefault(): Drive | null {
		return this.drives.find((d) => d.default_for_user != null) ?? null;
	}

	/** Drive whose root folder UUID matches `id`, or null. */
	findByRootFolderId(id: string | null | undefined): Drive | null {
		if (!id) return null;
		return this.drives.find((d) => d.root_folder_id === id) ?? null;
	}

	/** Drive whose own UUID matches `id`, or null. */
	findById(id: string | null | undefined): Drive | null {
		if (!id) return null;
		return this.drives.find((d) => d.id === id) ?? null;
	}
}

export const drives = new DrivesStore();

/**
 * Picker / breadcrumb icon for a drive:
 *   home   — default-personal (the user's home)
 *   folder — secondary personal drive
 *   users  — shared / team drive
 */
export function driveIcon(d: Drive): string {
	if (d.default_for_user) return 'home';
	return d.kind === 'shared' ? 'users' : 'folder';
}
