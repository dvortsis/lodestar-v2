// The Alerts endpoints (tag `library`) behind one small module, so the list, the detail and the
// dialogs share their types and their error mapping. The client never counts: `unread`, `alerts`
// and `read` are the server's.
import { api } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import { page } from '$app/state';
import { resolve } from '$app/paths';
import { withDensity } from '$lib/shell/nav';

export type SavedSearch = components['schemas']['SavedSearchView'];
export type AlertRow = components['schemas']['AlertRow'];
export type AlertsPage = components['schemas']['AlertsPage'];
export type AlertsEvent = components['schemas']['AlertsEvent'];

/** Alerts per request: the server's default, spelled out so a test can see it. */
export const PAGE_LIMIT = 50;
/** The most saved searches the server keeps (`409 too_many_saved_searches`). */
export const MAX_SAVED_SEARCHES = 1000;
/** The 40-hex info hash a `/t/<hash>` link may carry. */
export const INFO_HASH = /^[0-9a-f]{40}$/;

export const listSavedSearches = (signal?: AbortSignal) => api.get('/saved-searches', { signal });

export const getSavedSearch = (id: number, signal?: AbortSignal) =>
	api.get('/saved-searches/{id}', { path: { id }, signal });

export const listAlerts = (id: number, cursor?: string | null, signal?: AbortSignal) =>
	api.get('/saved-searches/{id}/alerts', {
		path: { id },
		query: { cursor: cursor ?? undefined, limit: PAGE_LIMIT },
		signal
	});

export const markAlertsRead = (savedSearch: number, through: number) =>
	api.post('/alerts/read', { body: { saved_search: savedSearch, through } });

export const setPaused = (id: number, paused: boolean) =>
	api.patch('/saved-searches/{id}', {
		path: { id },
		body: { paused } as components['schemas']['SavedSearchPatch']
	});

export const deleteSavedSearch = (id: number) =>
	api.delete('/saved-searches/{id}', { path: { id } });

export const createSavedSearch = (body: components['schemas']['SavedSearchCreate']) =>
	api.post('/saved-searches', { body });

// `resolve()` is typed by route id; these are dynamic strings, so they go through one id. There is
// no base path, so it returns the path unchanged (the shell does the same, see shell/nav.ts).
const resolved = (path: string) => resolve(path as '/search');

// Density is URL state (§10.3): links inside the app keep it, as the navigation does.
function plain(path: string): string {
	try {
		return withDensity(resolved(path), page.url.searchParams);
	} catch {
		return resolved(path); // outside a page (a test, say)
	}
}

/** `/alerts/<id>`: the id is an integer from the API. */
export const alertPath = (id: number) => plain(`/alerts/${Math.trunc(id)}`);

/** `/t/<hash>`, only for a 40-hex hash; anything else has no link. */
export const torrentPath = (hash: string) => (INFO_HASH.test(hash) ? plain(`/t/${hash}`) : null);

/** `/search?q=<q>` plus `&sort=<sort>` when the search has one (null = relevance). */
export function searchPath(s: Pick<SavedSearch, 'q' | 'sort'>): string {
	const params = new URLSearchParams({ q: s.q });
	if (s.sort !== null) params.set('sort', s.sort);
	return plain(`/search?${params}`);
}

/** The feed's path, served by the API (`GET /feeds/{id}`). */
export const feedPath = (id: number) => `/api/v1/feeds/${Math.trunc(id)}`;

export const alertsPath = () => plain('/alerts');
export const settingsSecurityPath = () => plain('/settings/security');

/** A positive integer route id, or null (`/alerts/abc`, `/alerts/1.5`, `/alerts/-1`). */
export function parseId(raw: string): number | null {
	if (!/^[0-9]{1,15}$/.test(raw)) return null;
	const n = Number(raw);
	return n > 0 ? n : null;
}
