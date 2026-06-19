<script lang="ts" module>
	/**
	 * Variable-height windowing list. Unlike {@link VirtualList} (uniform row
	 * pitch), each row declares its own `height`, so a single list can mix section
	 * headers and content rows of differing heights — e.g. the photo timeline's
	 * date headers and (square or justified) tile strips.
	 *
	 * It builds a prefix-sum offset table once per `rows` change and binary-searches
	 * the visible band on scroll, rendering only those rows (plus an overscan
	 * margin) and reserving the full height with a spacer so the scrollbar and any
	 * end-of-list sentinel behave exactly as with a fully-rendered list. Declared
	 * `height` MUST match the rendered row height or rows will drift.
	 */
	export interface VirtualRow {
		/** Rendered height of this row in px (incl. its own bottom gap). */
		height: number;
		/** Stable identity; keeps unchanged rows mounted as the window slides. */
		key?: string | number;
	}

	export interface VirtualRowsProps<T extends VirtualRow> {
		rows: T[];
		/** Extra pixels rendered above and below the viewport. */
		overscan?: number;
		windowClass?: string;
		windowStyle?: string;
		row: import('svelte').Snippet<[T, number]>;
	}
</script>

<script lang="ts" generics="T extends VirtualRow">
	import { onMount } from 'svelte';
	import { useVirtualWindow } from '$lib/composables/useVirtualWindow.svelte';

	let {
		rows,
		overscan = 600,
		windowClass = '',
		windowStyle = '',
		row
	}: VirtualRowsProps<T> = $props();

	let rootEl: HTMLDivElement;
	const vw = useVirtualWindow();

	// offsets[i] = Y of row i; offsets[rows.length] = total height.
	const offsets = $derived.by(() => {
		const o = new Array<number>(rows.length + 1);
		o[0] = 0;
		for (let i = 0; i < rows.length; i++) o[i + 1] = o[i] + rows[i].height;
		return o;
	});
	const totalHeight = $derived(offsets[rows.length] ?? 0);

	/** First index whose offset is > x. */
	function upperBound(arr: number[], x: number): number {
		let lo = 0;
		let hi = arr.length;
		while (lo < hi) {
			const mid = (lo + hi) >> 1;
			if (arr[mid] <= x) lo = mid + 1;
			else hi = mid;
		}
		return lo;
	}

	/** First index whose offset is >= x. */
	function lowerBound(arr: number[], x: number): number {
		let lo = 0;
		let hi = arr.length;
		while (lo < hi) {
			const mid = (lo + hi) >> 1;
			if (arr[mid] < x) lo = mid + 1;
			else hi = mid;
		}
		return lo;
	}

	const band = $derived.by(() => {
		const n = rows.length;
		if (n === 0) return { first: 0, last: 0, top: 0 };
		const topPx = vw.aboveBy - overscan;
		const botPx = vw.aboveBy + vw.viewportH + overscan;
		let first = upperBound(offsets, topPx) - 1; // row straddling/just above the top
		if (first < 0) first = 0;
		let last = lowerBound(offsets, botPx); // exclusive: first row starting at/after bottom
		if (last < first + 1) last = first + 1;
		if (last > n) last = n;
		return { first, last, top: offsets[first] };
	});

	const visible = $derived(rows.slice(band.first, band.last));

	onMount(() => vw.observe(rootEl));
</script>

<div bind:this={rootEl} class="vrows" style:height="{totalHeight}px">
	<div
		class="vrows__window {windowClass}"
		style="transform: translateY({band.top}px); {windowStyle}"
	>
		{#each visible as r, i (r.key ?? band.first + i)}
			{@render row(r, band.first + i)}
		{/each}
	</div>
</div>

<style>
	.vrows {
		position: relative;
		width: 100%;
	}

	.vrows__window {
		position: absolute;
		inset: 0 0 auto;
		will-change: transform;
	}
</style>
