// How a screen opens, closes and warms the detail drawer (§10.3, §10.5). The search page stays
// mounted under shallow routing, so the URL is /t/{hash} plus the search query string.
import { pushState, replaceState } from '$app/navigation';
import { resolve } from '$app/paths';
import { page } from '$app/state';
import { cacheHit, type SearchHit } from './cache';
import { watchDetailVisibility } from './visibility';

export interface OpenOptions {
	/** The search hit the drawer opens from; displayed only after visibility revalidation. */
	hit?: SearchHit;
	/** Where focus goes on close (the invoking row). */
	returnFocus?: () => HTMLElement | null | undefined;
	/** Replace the history entry even with no drawer open. A drawer that is already open always replaces. */
	replace?: boolean;
}

let pushed = false;
let returnTarget: (() => HTMLElement | null | undefined) | undefined;

export function openDetail(hash: string, options: OpenOptions = {}) {
	watchDetailVisibility();
	if (options.hit) cacheHit(hash, options.hit);
	if (options.returnFocus) returnTarget = options.returnFocus;
	// The query string rides along so the mounted search page keeps reading its query.
	const url = `${resolve('/t/[hash]', { hash })}${page.url.search}`;
	// One history entry per drawer session: a second torrent replaces the first, so Back or Esc
	// always leaves the drawer.
	if (page.state.detail || options.replace) {
		// eslint-disable-next-line svelte/no-navigation-without-resolve -- resolve() above, plus the query
		replaceState(url, { detail: { hash } });
	} else {
		// eslint-disable-next-line svelte/no-navigation-without-resolve -- resolve() above, plus the query
		pushState(url, { detail: { hash } });
		pushed = true;
	}
}

/** Close the drawer: Back when `openDetail` pushed the entry, otherwise back to /search. */
export function closeDetail() {
	if (pushed && page.state.detail) {
		pushed = false;
		history.back();
	} else {
		pushed = false;
		// eslint-disable-next-line svelte/no-navigation-without-resolve -- resolve() plus the query
		replaceState(`${resolve('/search')}${page.url.search}`, {});
	}
}

/** The host calls this when the drawer is gone for any reason (popstate included). */
export function markClosed() {
	pushed = false;
}

export const returnFocusTarget = () => returnTarget?.();

export { cancelDetailPrefetch, prefetchDetail } from './prefetch';
