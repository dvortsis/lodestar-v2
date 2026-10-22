import { formatBytes } from './bytes';
import { DEFAULT_LOCALE } from './shared';

export interface SizeBinEdges {
	/** Bin edges in bytes: 2^from … 2^to inclusive, ascending. */
	edges: number[];
	/** `formatBytes` of each edge, same order. */
	labels: string[];
}

/** U+2013 between two edge labels, with ordinary spaces: "1.00 GiB – 4.00 GiB". */
export const RANGE_SEPARATOR = ' – ';

/**
 * Labelled edges for a log-2 size histogram (metrics.md §5a): powers of 2 in bytes from 2^fromExp to
 * 2^toExp. Exponents are integers in 0…60; anything else (or from > to) gives no edges. The server
 * owns the actual bins; this only labels and never re-bins.
 */
export function sizeBinEdges(
	fromExp: number,
	toExp: number,
	locale = DEFAULT_LOCALE
): SizeBinEdges {
	const edges: number[] = [];
	const ok = (e: number) => Number.isInteger(e) && e >= 0 && e <= 60;
	if (ok(fromExp) && ok(toExp)) {
		for (let e = fromExp; e <= toExp; e++) edges.push(2 ** e);
	}
	return { edges, labels: edges.map((b) => formatBytes(b, locale)) };
}

/** "lo – hi" label for one size bin. Half-open `[lo, hi)`; the last bin is closed. */
export function sizeBinLabel(lo: number, hi: number, locale = DEFAULT_LOCALE): string {
	return `${formatBytes(lo, locale)}${RANGE_SEPARATOR}${formatBytes(hi, locale)}`;
}
