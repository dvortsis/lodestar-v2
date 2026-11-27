// What the status pill and the connection banner show, as pure functions of the event stream and
// the last crawler and rebuild snapshots. The pill never computes a count: every number is the
// server's, formatted by `$lib/format`, and unknown is null.
import type { components } from '$lib/api/schema';
import type { ApiError } from '$lib/api/client';
import type { EventsStatus } from '$lib/state/events.svelte';

export type CrawlerSnapshot = components['schemas']['CrawlerEvent'];
export type RebuildSnapshot = components['schemas']['RebuildEvent'];

export type PillKind =
	| 'unknown'
	| 'connecting'
	| 'reconnecting'
	| 'busy'
	| 'crawling'
	| 'idle'
	| 'paused'
	| 'rebuilding'
	| 'failed'
	| 'starting'
	| 'warming'
	| 'stalled'
	| 'restart';

/** Why the pill says "Stalled": the API's own reason, or `no_progress` (nothing stored for a window). */
export type StallReason = 'writer_stalled' | 'cohort_stalled' | 'index_lag' | 'no_progress';

/**
 * What the progress facts add to the pill (`progress.ts` decides); null = nothing to add or no
 * facts. `settled` = the start grace is over (the crawler reads what that means for it).
 */
export type ProgressOverlay =
	| { kind: 'starting' }
	| { kind: 'warming' }
	| { kind: 'settled' }
	| { kind: 'stalled'; reason: StallReason }
	| null;

export interface PillState {
	kind: PillKind;
	/** Torrents in the store (crawling, idle, paused); null when unknown. */
	torrents: number | null;
	/** Rebuild progress; null outside a rebuild. */
	done: number | null;
	total: number | null;
	/** Set when `kind` is `stalled`. */
	reason?: StallReason;
	/** Keys found and torrents stored since the start (`warming` only); null = unknown. */
	found?: number | null;
	stored?: number | null;
}

const REBUILDING = new Set<RebuildSnapshot['phase']>(['building', 'finishing', 'swapping']);

export function pillState(input: {
	stream: EventsStatus;
	crawler: CrawlerSnapshot | null;
	rebuild: RebuildSnapshot | null;
	progress?: ProgressOverlay;
	/** Saved settings wait for a restart (`GET /system` `restart_required`, §10.10.6). */
	restartNeeded?: boolean;
}): PillState {
	const { stream, crawler, rebuild, progress, restartNeeded } = input;
	const none = { torrents: null, done: null, total: null };
	const has = crawler !== null || rebuild !== null;
	if (stream === 'busy') return { kind: 'busy', ...none };
	if (stream === 'offline' || (stream === 'connecting' && has))
		return { kind: 'reconnecting', ...none };
	if (stream === 'connecting') return { kind: 'connecting', ...none };
	if (rebuild && REBUILDING.has(rebuild.phase))
		return { kind: 'rebuilding', torrents: null, done: rebuild.done, total: rebuild.total };
	if (rebuild?.phase === 'failed') return { kind: 'failed', ...none };
	if (!crawler) return { kind: 'unknown', ...none };
	if (crawler.paused && !restartNeeded)
		return { kind: 'paused', torrents: crawler.torrents, done: null, total: null };
	// Starting: a start inside its grace with no routing nodes yet (null is "off" or "not reported"),
	// or sampling held back by the server. Warming up: a start with nothing stored yet. Past the
	// grace, a DHT that never joined with nothing stored is a stall, not a calm "Idle".
	const k = progress?.kind;
	const fresh = crawler.stored === 0;
	const joining = crawler.dht_nodes === 0;
	if (k === 'settled' && joining && fresh)
		return { ...none, kind: 'stalled', reason: 'no_progress', torrents: crawler.torrents };
	const start =
		k === 'starting' || (k === 'warming' && joining)
			? 'starting'
			: k === 'warming' && fresh
				? 'warming'
				: null;
	if (start)
		return {
			...none,
			kind: start,
			torrents: crawler.torrents,
			...(start === 'warming' && { found: crawler.harvested, stored: crawler.stored })
		};
	const working = crawler.fetch_active > 0 || crawler.fetch_pending > 0;
	// An idle crawler has no writer to be stuck on. A starved pipeline is idle too, though, so the
	// stored-rate verdict (`no_progress`) and the other reasons stand.
	if (progress?.kind === 'stalled' && (working || progress.reason !== 'writer_stalled'))
		return { ...none, torrents: crawler.torrents, ...progress };
	// Only over the calm states: a stall, a rebuild or a lost connection matter more.
	if (restartNeeded) return { ...none, kind: 'restart', torrents: crawler.torrents };
	return {
		kind: working ? 'crawling' : 'idle',
		torrents: crawler.torrents,
		done: null,
		total: null
	};
}

export type ConnectionKind = 'ok' | 'reconnecting' | 'unreachable';

/**
 * `unreachable`: the server never answered since the page opened (a full-page state, since there is
 * nothing to show; a proxy that holds only the stream open is not this, see `restOk`). `reconnecting`: it answered before and went quiet (a banner; the page and
 * whatever the person was doing stay). A clean close without an error is only a reconnect.
 */
export function connectionKind(input: {
	stream: EventsStatus;
	error: ApiError | undefined;
	everLive: boolean;
	/** Some other request got an answer (REST works even if the stream is held up). */
	restOk?: boolean;
}): ConnectionKind {
	// A retry is `connecting` with the last failure still recorded: it is the same outage, not a
	// fresh start, so the state must not flicker (and the page must not remount) on every attempt.
	const retrying = input.stream === 'connecting' && input.error !== undefined;
	if (input.stream !== 'offline' && !retrying) return 'ok';
	if (input.restOk && !input.everLive) return 'ok';
	if (input.everLive) return 'reconnecting';
	return input.error ? 'unreachable' : 'ok';
}
