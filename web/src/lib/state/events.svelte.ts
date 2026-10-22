// One SSE stream per tab (§9 `/events`), read with `fetch` so a 401 or 429 keeps its status and
// `Retry-After` (EventSource hides both).
//
// Delivery rules:
// - Events are queued and handed to handlers once per animation frame, in arrival order.
// - Several `crawler` snapshots in one frame collapse to the latest (a snapshot supersedes the
//   previous one). `torrents` events are all delivered: each carries a count. Others are kept.
// - `resync` goes through the same queue, so it keeps its place in the order, and calls every
//   consumer's `onResync`.
// Reconnect: `Last-Event-ID`, the server's `retry:` as the base, doubled per consecutive failure
// with jitter (50-100 %), capped at 30 s. A clean close, a network error or 35 s of silence
// (2 x the 15 s heartbeat + slack) reconnects. 401 stops; 429 waits `Retry-After` and shows
// `busy`. A tab hidden for 10 s closes its stream (the server allows 4 per session) and
// reopens when visible again.
import { ApiError, errorFromResponse, handleUnauthorized, isAbort } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import { SseParser } from './sse';

type Event = components['schemas']['Event'];
export type Topic = Exclude<Event['event'], 'resync'>;
export type EventData<T extends Topic> = Extract<Event, { event: T }>['data'];
export type EventsStatus = 'connecting' | 'live' | 'busy' | 'offline' | 'stopped';

export const HIDDEN_CLOSE_MS = 10_000;
export const HEARTBEAT_TIMEOUT_MS = 35_000;
export const BACKOFF_CAP_MS = 30_000;
const DEFAULT_RETRY_MS = 5_000;
const URL_PATH = '/api/v1/events';

type Handler = (data: never) => void;
type Consumer = { topic: Topic; handler: Handler; onResync?: () => void };
type Queued = { event: string; data: unknown };

export type EventsDeps = {
	random: () => number;
	raf: (cb: () => void) => void;
};

export function createEvents(deps: Partial<EventsDeps> = {}) {
	const random = deps.random ?? Math.random;
	const raf = deps.raf ?? ((cb) => requestAnimationFrame(() => cb()));

	let status = $state<EventsStatus>('stopped');
	let error = $state<ApiError | undefined>();

	// Plain collection on purpose: nothing renders from it, only `status` and `error` are reactive.
	// eslint-disable-next-line svelte/prefer-svelte-reactivity
	const consumers = new Set<Consumer>();
	let queue: Queued[] = [];
	let frameScheduled = false;

	let ctl: AbortController | undefined;
	let connectedTopics = '';
	let lastId: string | undefined;
	let retryMs = DEFAULT_RETRY_MS;
	let failures = 0;
	let retryTimer: ReturnType<typeof setTimeout> | undefined;
	let silenceTimer: ReturnType<typeof setTimeout> | undefined;
	let hiddenTimer: ReturnType<typeof setTimeout> | undefined;
	let suspended = false; // closed because the tab was hidden
	let halted = false; // 401 or a request the server will never accept: no retry
	let refreshQueued = false;
	let listening = false;

	const topicsParam = () => {
		const topics: Topic[] = [];
		for (const c of consumers) if (!topics.includes(c.topic)) topics.push(c.topic);
		return topics.sort().join(',');
	};
	const isHidden = () => typeof document !== 'undefined' && document.visibilityState === 'hidden';

	function close() {
		ctl?.abort();
		ctl = undefined;
		clearTimeout(retryTimer);
		clearTimeout(silenceTimer);
		retryTimer = undefined;
	}

	function deliver() {
		frameScheduled = false;
		const batch = queue;
		queue = [];
		// Keep only the last crawler snapshot, at its own position.
		const lastCrawler = batch.findLastIndex((q) => q.event === 'crawler');
		for (const [i, q] of batch.entries()) {
			if (q.event === 'crawler' && i !== lastCrawler) continue;
			for (const c of [...consumers]) {
				try {
					if (q.event === 'resync') c.onResync?.();
					else if (c.topic === q.event) c.handler(q.data as never);
				} catch (e) {
					globalThis.reportError?.(e);
				}
			}
		}
	}

	function enqueue(item: Queued) {
		queue.push(item);
		if (!frameScheduled) {
			frameScheduled = true;
			raf(deliver);
		}
	}

	function scheduleReconnect(wait: number, next: EventsStatus) {
		status = next;
		clearTimeout(silenceTimer);
		clearTimeout(retryTimer);
		retryTimer = setTimeout(() => {
			retryTimer = undefined;
			void connect();
		}, wait);
	}

	function backoff() {
		const exp = Math.min(BACKOFF_CAP_MS, retryMs * 2 ** Math.min(failures, 16));
		failures++;
		return Math.round(exp * (0.5 + random() / 2));
	}

	async function connect() {
		close();
		if (!consumers.size || suspended || halted) return;
		const c = (ctl = new AbortController());
		let timedOut = false;
		connectedTopics = topicsParam();
		status = 'connecting';
		const headers: Record<string, string> = { Accept: 'text/event-stream' };
		if (lastId) headers['Last-Event-ID'] = lastId;
		try {
			const res = await fetch(`${URL_PATH}?topics=${encodeURIComponent(connectedTopics)}`, {
				headers,
				credentials: 'same-origin',
				signal: c.signal
			});
			if (!res.ok || !res.body) {
				const e = await errorFromResponse(res);
				if (c.signal.aborted) return;
				error = e;
				if (e.status === 401) {
					handleUnauthorized(e, '/events');
					halted = true;
					status = 'stopped';
				} else if (e.status === 429) {
					scheduleReconnect((e.retryAfter ?? Math.ceil(backoff() / 1000)) * 1000, 'busy');
				} else if (e.status >= 400 && e.status < 500 && e.status !== 408) {
					halted = true; // e.g. unknown_topic: retrying can't help
					status = 'stopped';
				} else {
					scheduleReconnect(backoff(), 'offline');
				}
				return;
			}
			status = 'live';
			error = undefined;
			await read(res.body, c, () => {
				timedOut = true;
				c.abort();
			});
			if (c.signal.aborted && !timedOut) return;
			scheduleReconnect(backoff(), 'offline'); // clean close, or silence
		} catch (e) {
			if ((c.signal.aborted && !timedOut) || (isAbort(e) && !timedOut)) return;
			error = new ApiError(0, 'network', e instanceof Error ? e.message : 'network error');
			scheduleReconnect(backoff(), 'offline');
		}
	}

	async function read(body: ReadableStream<Uint8Array>, c: AbortController, onSilence: () => void) {
		const reader = body.getReader();
		const decoder = new TextDecoder();
		const parser = new SseParser();
		c.signal.addEventListener('abort', () => void reader.cancel().catch(() => {}), { once: true });
		const arm = () => {
			clearTimeout(silenceTimer);
			silenceTimer = setTimeout(onSilence, HEARTBEAT_TIMEOUT_MS);
		};
		arm();
		let gotBytes = false;
		for (;;) {
			const { done, value } = await reader.read();
			if (done) return;
			arm();
			if (!gotBytes) {
				gotBytes = true;
				failures = 0;
			}
			for (const item of parser.push(decoder.decode(value, { stream: true }))) {
				if (item.type === 'retry') {
					retryMs = item.ms;
				} else {
					if (item.id) lastId = item.id;
					if (item.event === 'resync') {
						enqueue({ event: 'resync', data: undefined });
						continue;
					}
					try {
						enqueue({ event: item.event, data: JSON.parse(item.data) });
					} catch {
						// a malformed payload is skipped, the stream carries on
					}
				}
			}
		}
	}

	function refresh() {
		if (refreshQueued) return;
		refreshQueued = true;
		queueMicrotask(() => {
			refreshQueued = false;
			if (!consumers.size) {
				close();
				lastId = undefined;
				halted = false;
				suspended = false;
				status = 'stopped';
				return;
			}
			if (suspended) return;
			if (halted) return;
			if (retryTimer) return; // waiting out a backoff or Retry-After: connect() reads the topics then
			if (!ctl || topicsParam() !== connectedTopics) {
				failures = 0;
				void connect();
			}
		});
	}

	function onVisibility() {
		clearTimeout(hiddenTimer);
		hiddenTimer = undefined;
		if (isHidden()) {
			hiddenTimer = setTimeout(() => {
				hiddenTimer = undefined;
				if (!isHidden() || !consumers.size) return;
				suspended = true;
				close();
				status = 'stopped';
			}, HIDDEN_CLOSE_MS);
		} else if (suspended) {
			suspended = false;
			failures = 0;
			void connect();
		} else if (queue.length && !frameScheduled) {
			frameScheduled = true;
			raf(deliver);
		}
	}

	function listen(on: boolean) {
		if (typeof document === 'undefined' || on === listening) return;
		listening = on;
		if (on) document.addEventListener('visibilitychange', onVisibility);
		else {
			document.removeEventListener('visibilitychange', onVisibility);
			clearTimeout(hiddenTimer);
			hiddenTimer = undefined;
		}
	}

	return {
		get status() {
			return status;
		},
		get error() {
			return error;
		},
		/** Subscribes to a topic; returns the unsubscribe. The first consumer opens the stream. */
		on<T extends Topic>(
			topic: T,
			handler: (data: EventData<T>) => void,
			opts: { onResync?: () => void } = {}
		): () => void {
			const consumer: Consumer = { topic, handler: handler as Handler, onResync: opts.onResync };
			consumers.add(consumer);
			listen(true);
			refresh();
			return () => {
				if (!consumers.delete(consumer)) return;
				if (!consumers.size) listen(false);
				refresh();
			};
		}
	};
}

export const events = createEvents();
