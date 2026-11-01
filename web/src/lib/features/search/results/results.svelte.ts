// The search page's results (§10.4): one store, fed by W5's query (`ast`, `sort`, `hidden`, `exact`),
// read by the list (here) and the toolbar (S-search-3: `total`, `totalApprox`, `status`).
//
// - Latest wins (§10.12): a newer query aborts the older request, and an answer that arrives late is
//   dropped.
// - Refining keeps the previous rows (§10.4 "Refining"); an error keeps them too ("The last results
//   stay visible"), and so does a parse error: W5 clears `ast`, and the list keeps showing the last
//   query that parsed, as the strip does.
// - Paging follows `next_cursor`. An expired or unreadable cursor starts the query again from the top,
//   keeping the rows while it does, and says so once (`expired`).
// - The same query again is not a new request: W5 sets `ast` before it writes the URL, and any URL
//   change (density, a resubmit of the same text) re-runs the panel's effect. Only `retry` reloads.
import { untrack } from 'svelte';
import { ApiError, api, isAbort, latest } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import type { Ast } from '$lib/state/search.svelte';
import type { SearchHit } from './ResultRow.svelte';

type Notice = components['schemas']['SearchNotice'];
type Recovery = components['schemas']['SearchRecovery'];

export const PAGE = 50;
/** The API's largest page (`MAX_LIMIT`, crates/api/src/search.rs). */
const MAX_LIMIT = 200;

/** What the results depend on. `ast` null means no query (the empty state). */
export interface ResultsQuery {
	ast: Ast | null;
	/** Frontend-only opt-in to an empty match-all query. Never sent to the HTTP API. */
	browse?: boolean;
	sort: string;
	hidden: boolean;
	exact: boolean;
}

export type Status =
	/** No query: the empty state. */
	| 'empty'
	/** The first answer for a query hasn't arrived (no rows to keep). */
	| 'loading'
	| 'ready'
	/** The first request failed and there are no rows to keep. */
	| 'error';

const keyOf = (q: ResultsQuery) =>
	JSON.stringify([q.ast, q.sort, q.hidden, q.exact, q.browse ?? false]);
const empty = (q: ResultsQuery) => q.ast === null || (q.ast.op === 'all' && !q.browse);

export function createResults() {
	let hits = $state.raw<SearchHit[]>([]);
	let total = $state(0);
	let totalApprox = $state(false);
	let notices = $state.raw<Notice[]>([]);
	/** Per-clause counts of an empty first page (zero-result recovery). */
	let recovery = $state.raw<Recovery | null>(null);
	let status = $state<Status>('empty');
	/** A request for a new query is out while the previous rows stay (the hairline). */
	let refining = $state(false);
	/** The latest request's failure; the rows (if any) are the previous query's. */
	let error = $state.raw<ApiError | null>(null);
	let cursor = $state<string | null>(null);
	/** The first page's cursor: it names the index snapshot the rows came from (`/facets` lease). */
	let lease = $state<string | null>(null);
	let loadingMore = $state(false);
	let moreError = $state.raw<ApiError | null>(null);
	/** The query the rows answer, as a key; a reload of the same query keeps them. */
	let shownKey = $state<string | null>(null);
	/** The query of the request in flight, and of the last one answered or failed, as keys. */
	let inflightKey: string | null = null;
	let settledKey: string | null = null;
	/** The rows were reloaded from the top because the cursor expired (cleared by the next query). */
	let expired = $state(false);

	/** A polite message for the live region (e.g. after a Block); the panel reads and clears it. */
	let note = $state('');
	const slot = latest();
	let current: ResultsQuery | null = null;

	const body = (q: ResultsQuery, after: string | null, limit = PAGE) => ({
		ast: q.ast,
		q: null,
		sort: q.sort,
		limit,
		offset: null,
		cursor: after,
		hidden: q.hidden,
		exact: q.exact,
		tz: null
	});

	// Untracked: the list calls `load` from an effect, and reading the rows here would make that
	// effect depend on them, so every answer would load the query again.
	const load = (q: ResultsQuery) => untrack(() => run(q, false));

	async function run(q: ResultsQuery, force: boolean, keep = 0) {
		const key = empty(q) ? null : keyOf(q);
		// The query already shown, or already asked for: nothing to do (see the header).
		if (!force && key !== null) {
			if (key === inflightKey) return;
			if (key === settledKey && inflightKey === null) return; // a failure waits for `retry`
		}
		if (key !== shownKey) expired = false;
		current = q;
		cursor = null;
		moreError = null;
		loadingMore = false;
		if (empty(q)) {
			// The bare page, or a query with nothing in it: no request.
			slot.cancel();
			hits = [];
			total = 0;
			totalApprox = false;
			notices = [];
			recovery = null;
			error = null;
			refining = false;
			shownKey = null;
			inflightKey = null;
			settledKey = null;
			lease = null;
			status = 'empty';
			return;
		}
		refining = hits.length > 0;
		if (!refining) status = 'loading';
		error = null;
		inflightKey = key;
		try {
			// A reload that keeps the place refetches as many rows as were shown, so the scroll holds:
			// in the API's largest pages (MAX_LIMIT), not the list's.
			const size = keep > PAGE ? MAX_LIMIT : PAGE;
			let r = await slot.run((signal) =>
				api.post('/search', { body: body(q, null, size), signal })
			);
			let rows = r.hits;
			while (rows.length < keep && r.next_cursor) {
				const after = r.next_cursor;
				r = await slot.run((signal) => api.post('/search', { body: body(q, after, size), signal }));
				rows = [...rows, ...r.hits];
			}
			hits = rows;
			total = r.total;
			totalApprox = r.total_approx;
			notices = r.notices;
			recovery = r.recovery ?? null;
			cursor = r.next_cursor;
			lease = r.next_cursor;
			shownKey = key;
			settledKey = key;
			status = 'ready';
		} catch (e) {
			if (isAbort(e)) return; // a newer query owns the state
			settledKey = key;
			error = e instanceof ApiError ? e : new ApiError(0, 'unknown', String(e));
			if (hits.length === 0) status = 'error';
		} finally {
			if (current === q) {
				refining = false;
				inflightKey = null;
			}
		}
	}

	async function loadMore() {
		const q = current;
		if (!q || !cursor || loadingMore) return;
		loadingMore = true;
		moreError = null;
		try {
			const r = await slot.run((signal) => api.post('/search', { body: body(q, cursor), signal }));
			if (current !== q) return;
			hits = [...hits, ...r.hits];
			cursor = r.next_cursor;
		} catch (e) {
			if (isAbort(e)) return;
			const err = e instanceof ApiError ? e : new ApiError(0, 'unknown', String(e));
			// Results changed under the cursor (10 minutes idle, a toggle): start again from the top.
			if (err.code === 'cursor_expired' || err.code === 'bad_cursor') {
				loadingMore = false;
				expired = true;
				await run(q, true);
				return;
			}
			moreError = err;
		} finally {
			if (current === q) loadingMore = false;
		}
	}

	return {
		get hits() {
			return hits;
		},
		get total() {
			return total;
		},
		get totalApprox() {
			return totalApprox;
		},
		get notices() {
			return notices;
		},
		get recovery() {
			return recovery;
		},
		get status() {
			return status;
		},
		get refining() {
			return refining;
		},
		get error() {
			return error;
		},
		get hasMore() {
			return cursor !== null && moreError === null;
		},
		get loadingMore() {
			return loadingMore;
		},
		get moreError() {
			return moreError;
		},
		/** The first page's `next_cursor`, for `/facets` to count the same snapshot (null: one page). */
		get lease() {
			return lease;
		},
		/** The key of the query the rows answer (the list resets its scroll when it changes). */
		get shown() {
			return shownKey;
		},
		get note() {
			return note;
		},
		set note(v: string) {
			note = v;
		},
		/** The rows were reloaded from the top because the cursor expired. */
		get expired() {
			return expired;
		},
		/** The rows answer this query (false while a different query refines, or after an error). */
		showing(q: ResultsQuery) {
			return shownKey === keyOf(q);
		},
		load,
		loadMore: () => untrack(loadMore),
		/**
		 * The panel unmounted (the person left /search): what was answered before can be stale when
		 * they come back, and may no longer be allowed (a setting changed, e.g. "Show adult content"
		 * turned off). So the rows go too, and the next `load` starts from loading, not from the old
		 * rows under the refine hairline (which an error would then keep on screen).
		 */
		forget() {
			slot.cancel();
			inflightKey = settledKey = null;
			hits = [];
			total = 0;
			totalApprox = false;
			notices = [];
			recovery = null;
			error = null;
			cursor = null;
			lease = null;
			moreError = null;
			shownKey = null;
			expired = false;
			refining = false;
			loadingMore = false;
			status = 'empty';
		},
		/** The current query again from the top, whatever failed before (the live banner's Show). */
		reload(keep = 0) {
			return current ? untrack(() => run(current!, true, keep)) : Promise.resolve();
		},
		/** Retry: the current query again (after an error), or the next page. */
		retry() {
			if (moreError) {
				moreError = null;
				return loadMore();
			}
			return current ? untrack(() => run(current!, true)) : Promise.resolve();
		}
	};
}

export type Results = ReturnType<typeof createResults>;

/** The page's store, shared by the list and the toolbar (S-search-3). */
export const results = createResults();
