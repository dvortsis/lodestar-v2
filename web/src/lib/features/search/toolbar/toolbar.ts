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

export type View = 'list' | 'compact' | 'table' | 'cards';
export const VIEWS: readonly View[] = ['list', 'compact', 'table', 'cards'];

/** The account's Appearance density (§10.10), which sets the default view; it has no Table. */
export type Pref = 'comfortable' | 'compact' | 'cards';
const PREF_VIEW: Record<Pref, View> = { comfortable: 'list', compact: 'compact', cards: 'cards' };
export const prefOf = (v: unknown): Pref => (v === 'compact' || v === 'cards' ? v : 'comfortable');

/**
 * The one view the URL means: an explicit `view=table`, then an explicit `density=` (`comfortable`
 * is the List), then the account preference, then the List. Old links keep working.
 */
export function viewOf(url: URL, pref: Pref = 'comfortable'): View {
	const p = url.searchParams;
	if (p.get('view') === 'table') return 'table';
	const d = p.get('density');
	if (d === 'cards' || d === 'compact') return d;
	return d === 'comfortable' ? 'list' : PREF_VIEW[pref];
}

/** True when the URL says nothing about the view, so the account preference decides. */
export const viewUnset = (url: URL) =>
	url.searchParams.get('view') !== 'table' &&
	!['cards', 'compact', 'comfortable'].includes(url.searchParams.get('density') ?? '');

/**
 * This page with `view` chosen: it clears both parameters, then sets the one that view needs. With a
 * non-default preference the List is written out (`density=comfortable`), or the preference would
 * bring the old view back.
 */
export function viewHref(url: URL, view: View, pref: Pref = 'comfortable'): string {
	const u = new URL(url);
	u.searchParams.delete('density');
	u.searchParams.delete('view');
	if (view === 'table') u.searchParams.set('view', 'table');
	else if (view !== 'list') u.searchParams.set('density', view);
	else if (pref !== 'comfortable') u.searchParams.set('density', 'comfortable');
	return u.pathname + u.search + u.hash;
}
