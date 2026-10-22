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
	| 'rebuilding'
	| 'failed';

export interface PillState {
	kind: PillKind;
	/** Torrents in the store (crawling, idle); null when unknown. */
	torrents: number | null;
	/** Rebuild progress; null outside a rebuild. */
	done: number | null;
	total: number | null;
}

const REBUILDING = new Set<RebuildSnapshot['phase']>(['building', 'finishing', 'swapping']);

export function pillState(input: {
	stream: EventsStatus;
	crawler: CrawlerSnapshot | null;
	rebuild: RebuildSnapshot | null;
}): PillState {
	const { stream, crawler, rebuild } = input;
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
	const working = crawler.fetch_active > 0 || crawler.fetch_pending > 0;
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
	if (input.stream !== 'offline') return 'ok';
	if (input.restOk && !input.everLive) return 'ok';
	if (input.everLive) return 'reconnecting';
	return input.error ? 'unreachable' : 'ok';
}
