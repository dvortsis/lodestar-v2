// Pure brush logic for the histogram: a selection is a pair of bin edges `from < to` in 0…bins.
export interface BrushRange {
	from: number;
	to: number;
}

export type BrushKey = 'ArrowLeft' | 'ArrowRight' | 'Home' | 'End';

/** Move one edge, keeping at least one bin selected and staying inside 0…bins. */
export function moveEdge(
	range: BrushRange,
	edge: 'from' | 'to',
	key: BrushKey,
	bins: number
): BrushRange {
	const step = key === 'ArrowLeft' ? -1 : key === 'ArrowRight' ? 1 : 0;
	if (edge === 'from') {
		const next = key === 'Home' ? 0 : key === 'End' ? range.to - 1 : range.from + step;
		return { from: Math.max(0, Math.min(next, range.to - 1)), to: range.to };
	}
	const next = key === 'End' ? bins : key === 'Home' ? range.from + 1 : range.to + step;
	return { from: range.from, to: Math.min(bins, Math.max(next, range.from + 1)) };
}

/** The edge nearest to a pointer position `ratio` (0–1) across the plot. */
export function nearestEdge(ratio: number, bins: number): number {
	return Math.max(0, Math.min(bins, Math.round(ratio * bins)));
}

/** A drag from edge `a` to edge `b`, in either direction; a click (a === b) selects that bin. */
export function dragRange(a: number, b: number, bins: number): BrushRange {
	let from = Math.min(a, b);
	let to = Math.max(a, b);
	if (from === to) {
		if (to < bins) to += 1;
		else from -= 1;
	}
	return { from: Math.max(0, from), to: Math.min(bins, to) };
}

/** A range fitted to `bins`, or null when nothing of it remains (e.g. zero bins). */
export function clampRange(range: BrushRange, bins: number): BrushRange | null {
	if (bins < 1) return null;
	const from = Math.max(0, Math.min(range.from, bins - 1));
	const to = Math.max(from + 1, Math.min(range.to, bins));
	return { from, to };
}
