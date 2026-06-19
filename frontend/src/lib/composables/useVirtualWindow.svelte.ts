/**
 * Shared scroll-window tracker for windowing lists. Reactively reports how far a
 * list element's top has scrolled above the nearest scrollable ancestor's
 * viewport (`aboveBy`, px) and that viewport's height (`viewportH`).
 *
 * `VirtualList` (uniform rows) and `VirtualRows` (variable-height, section-aware
 * rows) each derive their own visible slice from these two signals, so the
 * scroll-ancestor detection and the rAF-throttled measurement live in exactly
 * one place. Ancestor-based (not its own scroll box) so it drops into an
 * existing scroll container without changing the single-scrollbar UX.
 */
export class VirtualWindow {
	/** Pixels of the list scrolled above the viewport top (negative until reached). */
	aboveBy = $state(0);
	/** Height of the scrollable viewport in px. */
	viewportH = $state(0);

	#root: HTMLElement | null = null;
	#scroller: HTMLElement | null = null;
	#ticking = false;

	/** Nearest scrollable ancestor, or null to mean the window/document. */
	#findScroller(el: HTMLElement): HTMLElement | null {
		let node = el.parentElement;
		while (node) {
			const oy = getComputedStyle(node).overflowY;
			if (oy === 'auto' || oy === 'scroll' || oy === 'overlay') return node;
			node = node.parentElement;
		}
		return null;
	}

	#measure = (): void => {
		const root = this.#root;
		if (!root) return;
		const rootTop = root.getBoundingClientRect().top;
		if (this.#scroller) {
			this.aboveBy = this.#scroller.getBoundingClientRect().top - rootTop;
			this.viewportH = this.#scroller.clientHeight;
		} else {
			this.aboveBy = -rootTop;
			this.viewportH = window.innerHeight;
		}
	};

	#onScroll = (): void => {
		if (this.#ticking) return;
		this.#ticking = true;
		requestAnimationFrame(() => {
			this.#ticking = false;
			this.#measure();
		});
	};

	/** Begin observing `root`; returns a teardown to call from `onMount`. */
	observe(root: HTMLElement): () => void {
		this.#root = root;
		this.#scroller = this.#findScroller(root);
		const target: EventTarget = this.#scroller ?? window;
		target.addEventListener('scroll', this.#onScroll, { passive: true });
		window.addEventListener('resize', this.#onScroll, { passive: true });
		const ro = new ResizeObserver(this.#onScroll);
		if (this.#scroller) ro.observe(this.#scroller);
		ro.observe(root);
		this.#measure();
		return () => {
			target.removeEventListener('scroll', this.#onScroll);
			window.removeEventListener('resize', this.#onScroll);
			ro.disconnect();
		};
	}

	/** Force a synchronous re-measure (e.g. just after rows first render). */
	remeasure(): void {
		this.#measure();
	}
}

export function useVirtualWindow(): VirtualWindow {
	return new VirtualWindow();
}
