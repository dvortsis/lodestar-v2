// What the status pill asks the server about progress. Loaded by the chrome, not by the pages that
// only read the rebuild snapshot, so the search route does not carry it.
import type { components } from '$lib/api/schema';
import { api } from '$lib/api/client';
import type { ProgressOverlay, StallReason } from './status';

/**
 * The startup sampling suppression (docs/metrics.md `dht_sample_suppression_seconds`; the CR-0
 * decision): up to six hours (plus the query timeout, covered by the extra minute) after a start
 * the crawler may legitimately store nothing. `GET /stats` says whether it is on now
 * (`sampling_blocked`, CR-0d); only when that isn't known does the pill compare `uptime_s` with
 * this bound.
 */
export const STARTUP_SUPPRESSION_S = 6 * 60 * 60 + 60;

/**
 * `ingest_per_hour` is the last full UTC hour, which can start up to two hours back, so the
 * zero-rate rule waits until that whole window lies after the suppression.
 */
const REPORTED_HOUR_S = 2 * 60 * 60;
const WINDOW_CLEAR_S = STARTUP_SUPPRESSION_S + REPORTED_HOUR_S;

export type SamplingBlocked = components['schemas']['SamplingBlocked'];

/** The progress facts the pill reads, taken from `GET /stats` and the Insights overview. */
export interface ProgressSnapshot {
	/**
	 * `sampling_blocked` (`GET /stats`): why the DHT holds back its samples; null = it doesn't (or
	 * the DHT is off); undefined = not known.
	 */
	sampling?: SamplingBlocked | null;
	/** The uptime at which sampling was last seen to stop being held back; null = not seen. */
	sampledFromS?: number | null;
	/** `fetched_per_hour` (`GET /stats`): server rate over its last 5 minutes; null = unmeasured. */
	fetchedPerHour: number | null;
	/** `ingest_per_hour` (Insights overview): the last full UTC hour; null = unmeasured. */
	ingestPerHour: number | null;
	/** `uptime_s` (`GET /stats`); null until known. */
	uptimeS: number | null;
	/** `kpi.health.reason` / `stale.reason` / `exceptions[].kind`, as the API reports them. */
	reasons: readonly string[];
}

/** In the order they win when several apply. */
const STALL_REASONS: readonly StallReason[] = ['writer_stalled', 'cohort_stalled', 'index_lag'];
/** The server raises these when nothing has been stored for a while, which a fresh start does too. */
const QUIET_REASONS: readonly string[] = ['writer_stalled', 'cohort_stalled'];

/**
 * The restart record can't be written, so sampling waits on that, not on the clock. (`locked` can
 * also be a filesystem without locks, which waits the usual six hours, so it isn't named here.)
 */
const RECORD_REASONS: readonly string[] = ['invalidate_failed'];

/**
 * After a start that samples at once (a resumed restart, or the DHT off), the first torrents take a
 * few minutes to be fetched and stored; the server's "nothing stored" reasons don't count before
 * this.
 */
export const START_GRACE_S = 15 * 60;

/** Sampling is held back: as the server says, or, when it doesn't say, by the uptime bound. */
const isYoung = (p: ProgressSnapshot) =>
	p.sampling !== undefined
		? p.sampling !== null
		: p.uptimeS !== null && p.uptimeS < STARTUP_SUPPRESSION_S;

/**
 * Whether a "nothing stored" reason counts: not while sampling is held back, nor in the grace after
 * the start or after sampling was last held back. `cohort_stalled` looks back over two full hours,
 * so after a suppression it waits that long.
 */
const quietCounts = (p: ProgressSnapshot, reason: string) => {
	if (isYoung(p)) return false;
	const up = p.uptimeS;
	if (up === null) return true;
	if (up < START_GRACE_S) return false;
	const grace = reason === 'cohort_stalled' ? REPORTED_HOUR_S : START_GRACE_S;
	return p.sampledFromS == null || up - p.sampledFromS >= grace;
};

/** The stored-rate window lies wholly after the start, and after sampling was last held back. */
const windowClear = (p: ProgressSnapshot) =>
	(p.uptimeS ?? 0) >= WINDOW_CLEAR_S &&
	!p.sampling &&
	(p.sampledFromS == null || (p.uptimeS ?? 0) - p.sampledFromS >= REPORTED_HOUR_S);

/**
 * A stall while the crawler runs: a reason the API names, or a stored rate of 0 over a window the
 * API reports that lies wholly after the startup suppression. A rate that isn't known is not a
 * stall, and fetching that doesn't reach the store is exactly the stall to catch, so the fetch
 * rate plays no part. Inside the suppression the server's "nothing stored" reasons
 * (`writer_stalled`, `cohort_stalled`) are what a fresh start looks like, so they don't count
 * there; `index_lag` always does.
 */
export function stallReason(p: ProgressSnapshot): StallReason | null {
	const named = STALL_REASONS.find(
		(r) => p.reasons.includes(r) && (!QUIET_REASONS.includes(r) || quietCounts(p, r))
	);
	// A starved idle pipeline carries `writer_stalled` too (the writer skips empty commits), so the
	// stored-rate verdict comes first or the idle rule would hide the very case it exists for.
	// Nothing stored is expected while sampling is held back, however long that lasts, nor in the
	// hours it covers.
	const starved = p.ingestPerHour === 0 && windowClear(p);
	if (starved && (!named || named === 'writer_stalled')) return 'no_progress';
	return named ?? null;
}

/** Under six hours up and either nothing flowing yet or the server's "nothing stored" reasons. */
const startingUp = (p: ProgressSnapshot) =>
	isYoung(p) &&
	(p.reasons.some((r) => QUIET_REASONS.includes(r)) ||
		!((p.ingestPerHour ?? 0) > 0 || (p.fetchedPerHour ?? 0) > 0));

/** The pill's extra state for these facts, if any. */
export function overlayOf(p: ProgressSnapshot): ProgressOverlay {
	const reason = stallReason(p);
	if (reason) return { kind: 'stalled', reason };
	if (!startingUp(p)) return null;
	return p.sampling && RECORD_REASONS.includes(p.sampling.reason)
		? { kind: 'starting', reason: 'restart_record' }
		: { kind: 'starting' };
}

/** `/stats` is asked this often; the rates are the server's, never computed here. */
const STATS_EVERY_MS = 60_000;
/** Coming back to the tab refreshes, but never more often than this. */
const MIN_GAP_MS = 10_000;
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
	let lastUptime = 0;
	// Whether the last `/stats` said sampling was held back, and the uptime it was last seen to stop.
	let held = false;
	let sampledFrom: number | null = null;
	return async function fetchProgress(now = Date.now()): Promise<ProgressSnapshot | null> {
		const stats = await api.get('/stats').catch(() => null);
		const odd =
			stats !== null && (stats.fetched_per_hour === 0 || (stats.index_lag_s ?? 0) > INDEX_LAG_S);
		const due = now - lastOverview >= OVERVIEW_EVERY_MS;
		const o =
			stats?.paused || !(odd || due)
				? null
				: await api.get('/insights/overview', { query: { range: '24h' } }).catch(() => null);
		// A restart (uptime went down) or a pause makes the kept stored-rate facts stale.
		if (stats && stats.uptime_s < lastUptime) [held, sampledFrom] = [false, null];
		if (stats?.paused || (stats && stats.uptime_s < lastUptime)) last = null;
		if (stats) lastUptime = stats.uptime_s;
		if (stats?.sampling_blocked) held = true;
		else if (stats?.sampling_blocked === null && held)
			[held, sampledFrom] = [false, stats.uptime_s];
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
			sampling: stats ? stats.sampling_blocked : undefined,
			sampledFromS: sampledFrom,
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
	let lastRun = -Infinity;
	const refresh = () => {
		if (typeof document !== 'undefined' && document.hidden) return;
		lastRun = Date.now();
		void source().then((p) => {
			if (!gone && p) set(overlayOf(p));
		});
	};
	refresh();
	const timer = setInterval(refresh, STATS_EVERY_MS);
	const onVisible = () => {
		if (Date.now() - lastRun >= MIN_GAP_MS) refresh();
	};
	document.addEventListener('visibilitychange', onVisible);
	return () => {
		gone = true;
		clearInterval(timer);
		document.removeEventListener('visibilitychange', onVisible);
	};
}
