// The live banner's controller (§10.4 "Live"). No query-scoped feed exists yet (B9), so it polls:
// every INTERVAL_MS, one `/search` with limit 1 for the query on screen, and compares its total
// with the total shown. "N new" is said only when both totals are exact and the new one is larger;
// an approximate total says nothing rather than guess. The list never changes by itself: Show is
// the caller's (it reloads from the top).
import { api, isAbort, latest } from '$lib/api/client';
import type { ResultsQuery } from '../results/results.svelte';

export const INTERVAL_MS = 30_000;
const DRIFT_MS = 1_000;

export interface LiveDeps {
	/** The query the rows answer, and its shown total (null: no rows, or not ready). */
	shown: () => { query: ResultsQuery; total: number; approx: boolean } | null;
	/** True while the user is scrolled deep or has a selection: no request, no change. */
	paused: () => boolean;
	/** Said politely when the count changes (at most once per interval). */
	announce?: (count: number) => void;
	intervalMs?: number;
}

export function createLive(deps: LiveDeps) {
	let fresh = $state(0);
	const slot = latest();
	/** The run in flight (a token, so a reset's late `finally` can't clear a newer run's flag). */
	let pending = 0;
	let runs = 0;
	let lastAnnounced = -Infinity;
	let sinceTotal: number | null = null;

	async function check() {
		const shown = deps.shown();
		if (!shown || shown.approx || pending || deps.paused()) return;
		if (typeof document !== 'undefined' && document.hidden) return;
		const { query: q, total } = shown;
		const mine = (pending = ++runs);
		try {
			const r = await slot.run((signal) =>
				api.post('/search', {
					body: {
						ast: q.ast,
						q: null,
						sort: q.sort,
						limit: 1,
						offset: null,
						cursor: null,
						hidden: q.hidden,
						exact: q.exact,
						tz: null
					},
					signal
				})
			);
			// The rows changed while it was out: the answer is about another query.
			const now = deps.shown();
			if (!now || now.total !== total || now.approx) return;
			const n = r.total_approx ? 0 : r.total - total;
			fresh = n > 0 ? n : 0;
			// A change the throttle held back is said by a later check, not lost.
			if (fresh > 0 && fresh !== sinceTotal) {
				const t = Date.now();
				// Slack for timer drift: a tick a little early must not wait a whole interval more.
				if (t - lastAnnounced >= (deps.intervalMs ?? INTERVAL_MS) - DRIFT_MS) {
					lastAnnounced = t;
					sinceTotal = fresh;
					deps.announce?.(fresh);
				}
			}
		} catch (e) {
			if (!isAbort(e)) fresh = 0; // a failure says nothing; the list's own errors are its own
		} finally {
			if (pending === mine) pending = 0;
		}
	}

	return {
		get fresh() {
			return fresh;
		},
		check,
		/** The rows were replaced (a new query, or Show): nothing is new any more. */
		reset() {
			slot.cancel();
			pending = 0;
			fresh = 0;
			sinceTotal = null;
		},
		/** Start polling; returns the stop. */
		start() {
			const timer = setInterval(() => void check(), deps.intervalMs ?? INTERVAL_MS);
			return () => {
				clearInterval(timer);
				slot.cancel();
			};
		}
	};
}

export type Live = ReturnType<typeof createLive>;
