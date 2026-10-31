// The live banner's controller (§10.4 "Live"). The query on screen is registered with the server
// (`POST /search/live`) and its count arrives as `search_live` events on the shared `/events`
// stream: `matched` is exact, on the same tier and visibility as the page, and costs no polling.
// The list never changes by itself: Show is the caller's (it reloads from the top).
// If registration fails (the 429 cap, offline, search disabled) the old probe takes over for that
// query: every INTERVAL_MS, one `/search` with limit 1, "N new" only when both totals are exact.
import { api, isAbort, latest } from '$lib/api/client';
import { events } from '$lib/state/events.svelte';
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
	let sinceCount: number | null = null;

	// The registration for the query shown. `reg` is a token: a slower POST for an older query
	// releases its own id instead of becoming current.
	let id: string | null = null;
	let key: string | null = null;
	let reg = 0;
	/** True when registration failed for the current query: the probe counts instead. */
	let polling = false;
	/** The latest `matched` for `id`, held back while paused and applied by the next tick. */
	let seen = 0;

	const release = (old: string | null) => {
		if (old) void api.delete('/search/live/{id}', { path: { id: old }, body: {} }).catch(() => {});
	};

	/** Says a changed count politely, at most once per interval; a held-back one waits for the next. */
	function note(n: number) {
		if (n <= 0 || n === sinceCount) return;
		const t = Date.now();
		// Slack for timer drift: a tick a little early must not wait a whole interval more.
		if (t - lastAnnounced >= (deps.intervalMs ?? INTERVAL_MS) - DRIFT_MS) {
			lastAnnounced = t;
			sinceCount = n;
			deps.announce?.(n);
		}
	}

	/** The probe (fallback only): re-run the query with limit 1 and compare exact totals. */
	async function check() {
		if (id) {
			if (seen > fresh && !deps.paused()) {
				fresh = seen;
				note(fresh);
			}
			return;
		}
		const shown = deps.shown();
		if (!polling || !shown || shown.approx || pending || deps.paused()) return;
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
			fresh = r.total_approx ? 0 : Math.max(0, r.total - total);
			note(fresh);
		} catch (e) {
			if (!isAbort(e)) fresh = 0; // a failure says nothing; the list's own errors are its own
		} finally {
			if (pending === mine) pending = 0;
		}
	}

	/**
	 * Registers `q` (null: none) and releases the previous registration. The same query again is a
	 * no-op unless `force` (Show: the count starts again from 0).
	 */
	async function follow(q: ResultsQuery | null, force = false) {
		const next = q?.ast ? JSON.stringify([q.ast, q.hidden]) : null;
		if (!force && next === key && (id || polling || reg)) return;
		const mine = ++reg;
		key = next;
		const old = id;
		id = null;
		polling = false;
		fresh = 0;
		seen = 0;
		sinceCount = null;
		release(old);
		if (!q?.ast) return;
		try {
			const r = await api.post('/search/live', {
				body: { ast: q.ast, q: null, hidden: q.hidden, tz: null }
			});
			if (mine !== reg) return release(r.id);
			id = r.id;
		} catch {
			if (mine === reg) polling = true;
		}
	}

	const current = () => {
		const s = deps.shown();
		return s ? s.query : null;
	};

	return {
		get fresh() {
			return fresh;
		},
		check,
		follow,
		/** The rows were replaced (a new query, or Show): nothing is new any more. */
		reset() {
			slot.cancel();
			pending = 0;
			fresh = 0;
			seen = 0;
			sinceCount = null;
		},
		/** Start listening (and, with no registration, polling); returns the stop. */
		start() {
			const again = () => void follow(current(), true);
			const offLive = events.on(
				'search_live',
				(e) => {
					// Another registration's count (another query of this session, or an old one).
					if (e.id !== id) return;
					seen = e.matched;
					if (deps.paused()) return;
					fresh = seen;
					note(fresh);
				},
				// A resync may mean the server forgot the registration (a restart): register again.
				{ onResync: again }
			);
			// A content save released every registration (the server clears them).
			const offSettings = events.on(
				'settings',
				(e) => {
					if (e.section === 'content') again();
				},
				{}
			);
			const timer = setInterval(() => void check(), deps.intervalMs ?? INTERVAL_MS);
			// A closing tab frees its registration now (the server's 30 s grace covers a failure).
			const hide = () => {
				if (!id) return;
				void fetch(`/api/v1/search/live/${encodeURIComponent(id)}`, {
					method: 'DELETE',
					headers: { 'Content-Type': 'application/json' },
					credentials: 'same-origin',
					keepalive: true
				}).catch(() => {});
			};
			addEventListener('pagehide', hide);
			return () => {
				removeEventListener('pagehide', hide);
				offLive();
				offSettings();
				clearInterval(timer);
				slot.cancel();
				++reg;
				key = null;
				release(id);
				id = null;
				polling = false;
			};
		}
	};
}

export type Live = ReturnType<typeof createLive>;
