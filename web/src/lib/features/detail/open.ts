// How a screen opens, closes and warms the detail drawer (§10.3, §10.5). The search page stays
// mounted under shallow routing, so the URL is /t/{hash} plus the search query string.
import { pushState, replaceState } from '$app/navigation';
import { resolve } from '$app/paths';
import { page } from '$app/state';
import { api, isAbort } from '$lib/api/client';
import { cacheDetail, cacheHit, cachedDetail, type SearchHit } from './cache';

export interface OpenOptions {
	/** The search hit the drawer opens from: its header and summary show at once. */
	hit?: SearchHit;
	/** Where focus goes on close (the invoking row). */
	returnFocus?: () => HTMLElement | null | undefined;
	/** Replace the history entry (j/k moving through results) instead of pushing one. */
	replace?: boolean;
}

let pushed = false;
let returnTarget: (() => HTMLElement | null | undefined) | undefined;

export function openDetail(hash: string, options: OpenOptions = {}) {
	if (options.hit) cacheHit(hash, options.hit);
	if (options.returnFocus) returnTarget = options.returnFocus;
	// The query string rides along so the mounted search page keeps reading its query.
	const url = `${resolve('/t/[hash]', { hash })}${page.url.search}`;
	if (options.replace && page.state.detail) {
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

const IDLE_MS = 150;
let timer: ReturnType<typeof setTimeout> | undefined;
let inflight: AbortController | undefined;

/**
 * Warm the cache for `hash` after 150 ms of idle. A newer call cancels a pending timer and an
 * in-flight request, so only the torrent the pointer or focus rests on is fetched.
 */
export function prefetchDetail(hash: string) {
	clearTimeout(timer);
	inflight?.abort();
	inflight = undefined;
	if (cachedDetail(hash)) return;
	timer = setTimeout(async () => {
		const ctl = (inflight = new AbortController());
		try {
			const detail = await api.get('/torrents/{hash}', { path: { hash }, signal: ctl.signal });
			if (!ctl.signal.aborted) cacheDetail(hash, detail);
		} catch (e) {
			if (!isAbort(e)) {
				// A prefetch is only a head start; the drawer shows the real error.
			}
		} finally {
			if (inflight === ctl) inflight = undefined;
		}
	}, IDLE_MS);
}
