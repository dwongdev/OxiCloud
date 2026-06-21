/**
 * Reactive cache of owner-id → display-name, with memoised parallel resolution.
 *
 * Replaces the identical `ownerNames` record + `resolveOwners()` block in the
 * favorites and recent views. The id→name resolver is injected so the cache
 * stays decoupled from any specific API endpoint.
 */
export class OwnerCache {
	#names = $state<Record<string, string>>({});
	#resolver: (id: string) => Promise<string>;

	constructor(resolver: (id: string) => Promise<string>) {
		this.#resolver = resolver;
	}

	/** Resolved names so far (id → display name). */
	get names(): Record<string, string> {
		return this.#names;
	}

	/** Display name for an id, or `null` when unknown/empty (for cell rendering). */
	name(id: string | null | undefined): string | null {
		if (!id) return null;
		return this.#names[id] ?? null;
	}

	/** Display name for an id, falling back to the id itself (for group labels). */
	label(id: string): string {
		return this.#names[id] ?? id;
	}

	/** Resolve every not-yet-cached id in parallel; nullish ids are skipped. */
	async resolve(ids: Iterable<string | null | undefined>): Promise<void> {
		// Transient scratch Set for dedup only — built, spread to an array, and
		// discarded in this call; never read reactively, so a plain Set is correct.
		// eslint-disable-next-line svelte/prefer-svelte-reactivity
		const pending = [...new Set([...ids].filter((id): id is string => !!id))].filter(
			(id) => !this.#names[id]
		);
		if (pending.length === 0) return;
		const resolved = await Promise.all(
			pending.map(async (id) => [id, await this.#resolver(id)] as const)
		);
		// One reactive assignment for the whole batch instead of one per id, so a
		// large resolve doesn't spread-copy the record N times (and re-run derives N times).
		this.#names = { ...this.#names, ...Object.fromEntries(resolved) };
	}
}

/** Create a reactive {@link OwnerCache} backed by `resolver`. */
export function useOwnerCache(resolver: (id: string) => Promise<string>): OwnerCache {
	return new OwnerCache(resolver);
}
