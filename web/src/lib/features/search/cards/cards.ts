// Cards density (§10.4): the list is virtual, so cards are laid out in rows of `cols` cards, and the
// list virtualizes those rows. The column count follows the list's own width (container, not
// viewport: the drawer takes space from it at xx-large).
export const CARD_MIN_PX = 280;
export const MAX_COLS = 3;

export function columns(width: number): number {
	if (!(width > 0)) return 1;
	return Math.min(MAX_COLS, Math.max(1, Math.floor(width / CARD_MIN_PX)));
}

export interface CardRow<T> {
	key: string;
	start: number;
	items: T[];
}

export function chunk<T extends { hash: string }>(items: readonly T[], cols: number): CardRow<T>[] {
	const out: CardRow<T>[] = [];
	for (let i = 0; i < items.length; i += cols) {
		const slice = items.slice(i, i + cols);
		out.push({ key: slice[0]!.hash, start: i, items: slice });
	}
	return out;
}

/** Calls `set` with the column count now and whenever the list's width changes; returns the stop. */
export function observeColumns(el: HTMLElement, set: (cols: number) => void): () => void {
	const ro = new ResizeObserver(() => set(columns(el.clientWidth)));
	ro.observe(el);
	return () => ro.disconnect();
}
