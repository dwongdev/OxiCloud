import { SvelteSet } from 'svelte/reactivity';

/**
 * Reactive multi-select over string ids. Backs the repeated
 * `let selected = $state(new Set()); function toggle(id) { … }` pattern used by
 * the photos grid, music picker and other list views with one source of truth.
 *
 * Backed by a reactive {@link SvelteSet}, so in-place mutations (`add`/`delete`)
 * drive `$derived`/template reads without copying the set.
 */
export class Selection {
	#ids = new SvelteSet<string>();

	/** The live selection set (read-only intent — mutate via the methods). */
	get ids(): SvelteSet<string> {
		return this.#ids;
	}

	get size(): number {
		return this.#ids.size;
	}

	get isEmpty(): boolean {
		return this.#ids.size === 0;
	}

	has(id: string): boolean {
		return this.#ids.has(id);
	}

	/** Selected ids as an array (e.g. for batch API calls). */
	values(): string[] {
		return [...this.#ids];
	}

	toggle(id: string): void {
		if (this.#ids.has(id)) this.#ids.delete(id);
		else this.#ids.add(id);
	}

	add(id: string): void {
		this.#ids.add(id);
	}

	delete(id: string): void {
		this.#ids.delete(id);
	}

	/** Replace the whole selection. */
	set(ids: Iterable<string>): void {
		this.#ids.clear();
		for (const id of ids) this.#ids.add(id);
	}

	clear(): void {
		this.#ids.clear();
	}
}

/** Create a reactive {@link Selection}. */
export function useSelection(): Selection {
	return new Selection();
}
