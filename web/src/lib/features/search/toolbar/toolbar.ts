// The toolbar's parameters (§10.4): the sorts W5 accepts (§7: relevance, discovered, size, files,
// each either way) and the density link. Pure, so they're tested without a DOM.
import { m } from '$lib/paraglide/messages.js';
import { DEFAULT_SORT } from '$lib/state/search.svelte';

/** The options, in menu order. A bare field is descending (the server's default order). */
export const SORTS: { value: string; label: () => string }[] = [
	{ value: DEFAULT_SORT, label: m.search_toolbar_sort_relevance },
	{ value: '-discovered', label: m.search_toolbar_sort_newest },
	{ value: '+discovered', label: m.search_toolbar_sort_oldest },
	{ value: '-size', label: m.search_toolbar_sort_largest },
	{ value: '+size', label: m.search_toolbar_sort_smallest },
	{ value: '-files', label: m.search_toolbar_sort_most_files },
	{ value: '+files', label: m.search_toolbar_sort_fewest_files }
];

/** The option for W5's sort: `discovered` (a pasted link) is `-discovered`. */
export function sortValue(sort: string): string {
	const v = sort === DEFAULT_SORT || /^[+-]/.test(sort) ? sort : `-${sort}`;
	return SORTS.some((s) => s.value === v) ? v : DEFAULT_SORT;
}

/** This page with the view parameter `key` set to `value`; the default drops the parameter. */
export function viewParamHref(url: URL, key: 'density' | 'view', value: string): string {
	const u = new URL(url);
	if (value === 'compact' || value === 'table' || value === 'cards') u.searchParams.set(key, value);
	else u.searchParams.delete(key);
	return u.pathname + u.search + u.hash;
}
export const densityHref = (url: URL, density: string) => viewParamHref(url, 'density', density);
