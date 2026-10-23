// The detail drawer (S-detail) opens with a shallow `pushState` to `/t/{hash}?<the search>`: the
// search page stays mounted and `search.q` still reads the query. W5 commits with
// `goto(<current path>?…)`, which from `/t/…` would navigate to the full detail page. So every
// search write from this page (setText, submit, setAst, setHidden, reset, and S-search-2/3's facet
// clicks and toolbar) calls this first: it puts the URL back on `/search`, which closes the drawer.
import { replaceState } from '$app/navigation';
import { page } from '$app/state';

export const SEARCH_PATH = '/search';

export function ensureSearchPath(): void {
	if (page.url.pathname === SEARCH_PATH) return;
	// Same route, same query: a shallow replace, so nothing reloads.
	// eslint-disable-next-line svelte/no-navigation-without-resolve
	replaceState(SEARCH_PATH + page.url.search, {});
}
