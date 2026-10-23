// Test doubles for the query bar's specs: a reactive stand-in for kit's `page` and a W5 `Nav` over
// it, so `search.q` and the bar's effects follow URL changes the way they do in the app.
import type { Nav } from '$lib/state/search.svelte';

let url = $state(new URL('http://x/search'));
let pageState = $state<Record<string, unknown>>({});

export const testPage = {
	get url() {
		return url;
	},
	get state() {
		return pageState;
	}
};

export const navCalls: { url: string; push: boolean }[] = [];

export const testNav: Nav = {
	url: () => url,
	go: async (to, push) => {
		navCalls.push({ url: to, push });
		url = new URL(to, url);
	}
};

/** Resets the URL (and page.state) between tests. */
export function visit(path: string, state: Record<string, unknown> = {}) {
	url = new URL(path, 'http://x');
	pageState = state;
	navCalls.length = 0;
}
