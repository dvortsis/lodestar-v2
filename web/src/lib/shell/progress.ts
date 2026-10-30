// What the status pill asks the server about progress. Loaded by the chrome, not by the pages that
// only read the rebuild snapshot, so the search route does not carry it.
import { api } from '$lib/api/client';
import type { ProgressOverlay, StallReason } from './status';

/**
 * The startup sampling suppression (docs/metrics.md `dht_sample_suppression_seconds`; the CR-0
 * decision): up to six hours after a start the crawler may legitimately store nothing. The API
 * does not expose the remaining time, so the pill compares the API's `uptime_s` with this bound.
 */
export const STARTUP_SUPPRESSION_S = 6 * 60 * 60;

/**
 * `ingest_per_hour` is the last full UTC hour, which can start up to two hours back, so the
 * zero-rate rule waits until that whole window lies after the suppression.
 */
const WINDOW_CLEAR_S = STARTUP_SUPPRESSION_S + 2 * 60 * 60;

/** The progress facts the pill reads, taken from `GET /stats` and the Insights overview. */
export interface ProgressSnapshot {
	/** `fetched_per_hour` (`GET /stats`): server rate over its last 5 minutes; null = unmeasured. */
	fetchedPerHour: number | null;
	/** `ingest_per_hour` (Insights overview): the last full UTC hour; null = unmeasured. */
	ingestPerHour: number | null;
	/** `uptime_s` (`GET /stats`); null until known. */
	uptimeS: number | null;
	/** `kpi.health.reason` / `stale.reason` / `exceptions[].kind`, as the API reports them. */
	reasons: readonly string[];
}

const STALL_REASONS: readonly string[] = ['writer_stalled', 'cohort_stalled', 'index_lag'];

/**
 * A stall while the crawler runs: a reason the API names, or a stored rate of 0 over a window the
 * API reports that lies wholly after the startup suppression. A rate that isn't known is not a
 * stall. Fetching that doesn't reach the store is exactly the stall to catch, so the fetch rate
 * plays no part.
 */
export function stallReason(p: ProgressSnapshot): StallReason | null {
	const named = p.reasons.find((r) => STALL_REASONS.includes(r));
	if (named) return named as StallReason;
	return p.ingestPerHour === 0 && (p.uptimeS ?? 0) >= WINDOW_CLEAR_S ? 'no_progress' : null;
}

/** Under six hours up with nothing flowing yet (or nothing known): the quiet is expected. */
const startingUp = (p: ProgressSnapshot) =>
	p.uptimeS !== null &&
	p.uptimeS < STARTUP_SUPPRESSION_S &&
	!((p.ingestPerHour ?? 0) > 0 || (p.fetchedPerHour ?? 0) > 0);

/** The pill's extra state for these facts, if any. */
export function overlayOf(p: ProgressSnapshot): ProgressOverlay {
	const reason = stallReason(p);
	if (reason) return { kind: 'stalled', reason };
	return startingUp(p) ? { kind: 'starting' } : null;
}

/** `/stats` is asked this often; the rates are the server's, never computed here. */
const STATS_EVERY_MS = 60_000;
/** The overview (a rollup query) is asked this often when `/stats` shows nothing odd. */
const OVERVIEW_EVERY_MS = 5 * 60_000;
/** The index is "lagging" past this (docs/metrics.md `index_lag`); the same bound the server uses. */
const INDEX_LAG_S = 30;

/**
 * `GET /stats` is cheap and asked every time: the fetch rate and uptime. The Insights overview
 * (stored rate and the server's own stall reasons) is a rollup query: it is asked at once when
 * `/stats` shows fetching at 0 or the index behind, and otherwise every few minutes, because a
 * writer can stall while fetches still count. It is not asked while paused. Either failing leaves
 * its facts unknown (null), which is never a stall.
 */
export function createProgressSource() {
	let lastOverview = Date.now();
	let last: ProgressSnapshot | null = null;
	return async function fetchProgress(now = Date.now()): Promise<ProgressSnapshot | null> {
		const stats = await api.get('/stats').catch(() => null);
		const odd =
			stats !== null && (stats.fetched_per_hour === 0 || (stats.index_lag_s ?? 0) > INDEX_LAG_S);
		const due = now - lastOverview >= OVERVIEW_EVERY_MS;
		const o =
			stats?.paused || !(odd || due)
				? null
				: await api.get('/insights/overview', { query: { range: '24h' } }).catch(() => null);
		if (stats?.paused) last = null;
		if (o) lastOverview = now;
		if (!stats && !o) return null;
		// Between overview calls the last stored-rate facts stand (they describe an hour, not a moment).
		const keep = !o && !stats?.paused ? last : null;
		const reasons: string[] = [];
		if (o) {
			if (o.kpi.health.reason) reasons.push(o.kpi.health.reason);
			if (o.stale) reasons.push(o.stale.reason);
			for (const e of o.exceptions) reasons.push(e.kind);
		}
		last = {
			fetchedPerHour: stats?.fetched_per_hour ?? null,
			ingestPerHour: o ? o.ingest_per_hour : (keep?.ingestPerHour ?? null),
			uptimeS: stats?.uptime_s ?? null,
			reasons: o ? reasons : (keep?.reasons ?? [])
		};
		return last;
	};
}

/** Asks now, every minute while the tab is visible, and when it becomes visible; returns the stop. */
export function startProgress(
	set: (p: ProgressOverlay) => void,
	source: () => Promise<ProgressSnapshot | null> = createProgressSource()
): () => void {
	let gone = false;
	const refresh = () => {
		if (typeof document !== 'undefined' && document.hidden) return;
		void source().then((p) => {
			if (!gone && p) set(overlayOf(p));
		});
	};
	refresh();
	const timer = setInterval(refresh, STATS_EVERY_MS);
	document.addEventListener('visibilitychange', refresh);
	return () => {
		gone = true;
		clearInterval(timer);
		document.removeEventListener('visibilitychange', refresh);
	};
}
