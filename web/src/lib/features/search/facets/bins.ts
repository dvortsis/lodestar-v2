// Histogram bins (`[lo, hi)`, the open top bin's `hi` null) and the query's inclusive ranges.
// A brush picks edges `from < to` (charts/brush.ts); the clause is `lo = bins[from].lo` and
// `hi = bins[to - 1].hi - 1`, with the histogram's ends as `rangeOfBrush` says.
import type { BrushRange } from '$lib/charts/brush';
import type { components } from '$lib/api/schema';

export type Bin = components['schemas']['HistogramBin'];

type Range = { lo: number | null; hi: number | null };

/**
 * The inclusive range a brush selects. An edge inside the histogram is that bin's edge. An edge at
 * the histogram's end keeps the clause's own bound (`current`) when that bound cut the histogram
 * there: Discovered's bins span only the current matches (histograms aren't disjunctive), so its
 * end bin can be the clause's edge, not the data's. Otherwise, and at an open top bin, the end is
 * unbounded. Null: no range.
 */
export function rangeOfBrush(
	bins: Bin[],
	brush: BrushRange,
	current: Range | null = null
): Range | null {
	const first = bins[brush.from];
	const last = bins[brush.to - 1];
	if (!first || !last) return null;
	// At an end: the clause's bound if it cut the histogram there (it lies at or past the end),
	// else nothing (the person dragged the edge out to the end).
	const lo =
		brush.from > 0 ? first.lo : current?.lo != null && current.lo <= first.lo ? current.lo : null;
	const hi =
		brush.to < bins.length
			? last.hi! - 1
			: last.hi !== null && current?.hi != null && current.hi >= last.hi - 1
				? current.hi
				: null;
	return lo === null && hi === null ? null : { lo, hi };
}

const DAY = 86_400;

/**
 * Whole UTC days around a range of seconds: the query language's dates are days, and the server
 * prints a date clause as days (crates/search query.rs), so anything finer wouldn't survive the
 * canonical text in the URL.
 */
export function wholeDays(range: Range | null): Range | null {
	if (!range) return null;
	return {
		lo: range.lo === null ? null : Math.floor(range.lo / DAY) * DAY,
		hi: range.hi === null ? null : Math.floor(range.hi / DAY) * DAY + DAY - 1
	};
}

/**
 * The brush that shows a range over these bins: the bins it overlaps. Null when there is no
 * range, or nothing of it falls on the bins.
 */
export function brushOfRange(
	bins: Bin[],
	range: { lo: number | null; hi: number | null } | null
): BrushRange | null {
	if (!range || bins.length === 0) return null;
	const lo = range.lo ?? -Infinity;
	const hi = range.hi ?? Infinity;
	let from = -1;
	let to = -1;
	bins.forEach((b, i) => {
		const top = b.hi === null ? Infinity : b.hi - 1;
		if (top >= lo && b.lo <= hi) {
			if (from < 0) from = i;
			to = i + 1;
		}
	});
	return from < 0 ? null : { from, to };
}

/** The bin edges: each bin's `lo`, then the last `hi` (null for an open top). */
export function edgesOf(bins: Bin[]): (number | null)[] {
	return bins.length ? [...bins.map((b) => b.lo), bins[bins.length - 1]!.hi] : [];
}
