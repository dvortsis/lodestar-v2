// The facet rail's counts (§10.4): `POST /facets` for the results' query, counted on the same
// index snapshot as the rows (`lease`, the results' first `next_cursor`). Counts are the server's,
// never computed here. One store follows the results (the rail); the staged bottom sheet makes
// its own for the query it hasn't applied yet.
// - Latest wins; the same request is never sent twice (see results.svelte.ts).
// - While a new request is out, the last counts stay (the rail doesn't blank between queries).
// - An expired or unfitting lease falls back to the index serving now.
import { untrack } from 'svelte';
import { ApiError, api, isAbort, latest } from '$lib/api/client';
import type { components } from '$lib/api/schema';
type Ast = components['schemas']['AstNode'];

export type FacetsResponse = components['schemas']['FacetsResponse'];
export type TermFacet = components['schemas']['TermFacet'];
export type Histogram = components['schemas']['Histogram'];

/** Values listed per term facet: the server's default, and "Show more" (its maximum). */
export const LIST = 8;
export const MORE = 100;

export interface FacetsQuery {
	ast: Ast | null;
	browse?: boolean;
	sort: string;
	hidden: boolean;
	lease: string | null;
	/** Per facet, values to list (default `LIST`). */
	limits?: Record<string, number>;
}

const keyOf = (q: FacetsQuery) =>
	JSON.stringify([q.ast, q.sort, q.hidden, q.lease, q.limits ?? {}, q.browse ?? false]);

export function createFacets() {
	let data = $state.raw<FacetsResponse | null>(null);
	let error = $state.raw<ApiError | null>(null);
	let loading = $state(false);
	let settledKey: string | null = null;
	let inflightKey: string | null = null;
	const slot = latest();

	/** A lease the server refused: sent as null from then on, until the results bring a new one. */
	let deadLease: string | null = null;

	async function run(q: FacetsQuery, force: boolean) {
		if (q.lease !== null && q.lease === deadLease) q = { ...q, lease: null };
		if (q.ast === null || (q.ast.op === 'all' && !q.browse)) {
			slot.cancel();
			data = null;
			error = null;
			loading = false;
			settledKey = inflightKey = null;
			return;
		}
		const key = keyOf(q);
		if (!force && (key === inflightKey || (key === settledKey && inflightKey === null))) return;
		inflightKey = key;
		loading = true;
		error = null;
		try {
			data = await slot.run((signal) =>
				api.post('/facets', {
					body: {
						ast: q.ast,
						q: null,
						sort: q.sort,
						hidden: q.hidden,
						tz: null,
						lease: q.lease,
						limits: q.limits ?? {}
					},
					signal
				})
			);
			settledKey = key;
		} catch (e) {
			if (isAbort(e)) return; // a newer request owns the state
			const err = e instanceof ApiError ? e : new ApiError(0, 'unknown', String(e));
			// The lease expired (10 minutes idle) or no longer fits (a setting changed): count the
			// index serving now instead, rather than failing on every Retry.
			if (q.lease !== null && (err.code === 'cursor_expired' || err.code === 'bad_cursor')) {
				if (inflightKey === key) inflightKey = null;
				deadLease = q.lease;
				return run({ ...q, lease: null }, true);
			}
			settledKey = key;
			error = err;
		} finally {
			if (inflightKey === key) {
				inflightKey = null;
				loading = false;
			}
		}
	}

	let last: FacetsQuery | null = null;
	return {
		get data() {
			return data;
		},
		get error() {
			return error;
		},
		get loading() {
			return loading;
		},
		/** Untracked: an effect that calls it doesn't depend on the counts. */
		load(q: FacetsQuery) {
			last = q;
			return untrack(() => run(q, false));
		},
		retry() {
			return last ? untrack(() => run(last!, true)) : Promise.resolve();
		}
	};
}

export type Facets = ReturnType<typeof createFacets>;
