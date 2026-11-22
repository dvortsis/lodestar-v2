// The toolbar's parameters (§10.4): the sorts W5 accepts (§7: relevance, discovered, size, files,
// each either way) and the density link. Pure, so they're tested without a DOM.
import { m } from '$lib/paraglide/messages.js';
import { DEFAULT_SORT } from '$lib/state/search.svelte';
import type { View } from './view';

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

/** The views, in switcher order. */
export const VIEWS: readonly View[] = ['list', 'compact', 'table', 'cards'];

/**
 * This page with `view` chosen: it clears both parameters, then sets the one that view needs. With a
 * non-default preference the List is written out (`density=comfortable`), or the preference would
 * bring the old view back.
 */
export function viewHref(url: URL, view: View, pref?: string): string {
	const u = new URL(url);
	u.searchParams.delete('density');
	u.searchParams.delete('view');
	if (view === 'table') u.searchParams.set('view', 'table');
	else if (view !== 'list') u.searchParams.set('density', view);
	else if (pref === 'compact' || pref === 'cards') u.searchParams.set('density', 'comfortable');
	return u.pathname + u.search + u.hash;
}

/**
 * The account's Appearance density is the default view. The shell applies it to `<html data-density>`
 * only after the first interaction, so a first paint would miss it: the cache the last visit left
 * (prefs.ts) is applied the same way now, if nothing has set it. The panel and the toolbar read the
 * attribute with the URL, so a later change (the shell's answer, the Appearance screen) takes
 * effect with the next query and never rearranges the list on screen. This module loads with
 * both, before either mounts.
 */
export function seedPref() {
	const root = document.documentElement.dataset;
	try {
		const d = JSON.parse(localStorage.getItem('lodestar.appearance') ?? '{}').density;
		if (!root.density && (d === 'compact' || d === 'cards')) root.density = d;
	} catch {
		/* storage blocked, or a bad cache: the shell's answer comes later */
	}
}

seedPref(); // at load: the chunk loads with the panel, before either mounts
