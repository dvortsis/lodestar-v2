// Selection helpers loaded with the checkboxes (Shift range, select all), kept out of the store
// the results panel always loads.
import { m } from '$lib/paraglide/messages.js';
import { selection } from './selection.svelte';

let anchor: string | null = null;

/** A row checkbox: Shift extends from the last one toggled, in list order. */
export function pick(
	event: MouseEvent & { currentTarget: HTMLInputElement },
	hash: string,
	rows: readonly string[]
) {
	event.stopPropagation(); // not the row's activation
	const a = anchor === null ? -1 : rows.indexOf(anchor);
	const b = rows.indexOf(hash);
	if (event.shiftKey && a >= 0 && b >= 0) {
		const have = new Set(selection.hashes);
		const [lo, hi] = a < b ? [a, b] : [b, a];
		selection.put([...selection.hashes, ...rows.slice(lo, hi + 1).filter((h) => !have.has(h))]);
	} else selection.toggle(hash, event.currentTarget.checked);
	anchor = hash;
}

/** Select all of `rows` (the loaded ones), or none of them when they all are already. */
export function selectAll(rows: readonly string[]) {
	const have = new Set(selection.hashes);
	const every = rows.length > 0 && rows.every((h) => have.has(h));
	selection.put(
		every
			? selection.hashes.filter((h) => !rows.includes(h))
			: [...selection.hashes, ...rows.filter((h) => !have.has(h))]
	);
}

/** The text the polite region reads when the count changes (§10.13). */
export function countText(n: number, format: (n: number) => string) {
	return n === 0
		? m.search_bulk_none()
		: (n === 1 ? m.search_bulk_count_one : m.search_bulk_count_other)({ count: format(n) });
}
