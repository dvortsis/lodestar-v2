// The live log's controller (§10.10.12). It opens with the ring buffer's recent lines (`GET /logs`),
// then follows `GET /logs/stream` from the newest id, read with `fetch` so a 401, 429 or 503 keeps
// its status and `Retry-After` (as `/events` does). The filters go with both requests, so the server
// filters. Lines are batched into one update per animation frame.
//
// - Following: new lines move the view only while `following`; scrolled up, they count in `unseen`.
// - Pause freezes what is shown; lines keep arriving into `held` and join on resume.
// - At most MAX_LINES are kept; older ones are dropped and `trimmed` counts them.
// - A hidden tab closes the stream after HIDDEN_CLOSE_MS and resumes from the last id on return.
// - Reconnects back off (doubling, jittered, capped), never a tight loop.
import { SvelteURLSearchParams } from 'svelte/reactivity';
import { ApiError, api, errorFromResponse, handleUnauthorized, isAbort } from '$lib/api/client';
import { SseParser } from '$lib/state/sse';
import { purge, type LogLevel, type LogLine } from './text';

export const MAX_LINES = 10_000;
export const PAGE = 500;
export const HIDDEN_CLOSE_MS = 5_000;
export const BACKOFF_BASE_MS = 1_000;
export const BACKOFF_CAP_MS = 30_000;
export const SILENCE_MS = 35_000;
export const CONNECT_TIMEOUT_MS = 15_000;
const URL_PATH = '/api/v1/logs/stream';

export type StreamStatus = 'connecting' | 'live' | 'reconnecting' | 'offline' | 'unavailable';

export interface Filters {
	level: LogLevel;
	components: string[];
	text: string;
}
export const NO_FILTERS: Filters = { level: 'trace', components: [], text: '' };

export interface LogViewDeps {
	raf: (cb: () => void) => void;
	random: () => number;
	hidden: () => boolean;
}

const query = (f: Filters) => ({
	level: f.level === 'trace' ? undefined : f.level,
	components: f.components.length ? f.components.join(',') : undefined,
	text: f.text.trim() ? f.text.trim().slice(0, 200) : undefined
});

export function createLogView(deps: Partial<LogViewDeps> = {}) {
	const raf = deps.raf ?? ((cb) => requestAnimationFrame(() => cb()));
	const random = deps.random ?? Math.random;
	const isHidden = deps.hidden ?? (() => typeof document !== 'undefined' && document.hidden);

	let lines = $state.raw<LogLine[]>([]);
	let held = $state.raw<LogLine[]>([]);
	let status = $state<StreamStatus>('connecting');
	let loaded = $state(false);
	let loadError = $state<ApiError | undefined>();
	let paused = $state(false);
	let following = $state(true);
	let unseen = $state(0);
	let trimmed = $state(0);
	let skipped = $state(0);
	let hasEarlier = $state(false);
	let loadingEarlier = $state(false);
	let earlierError = $state(false);
	let filters = $state<Filters>(NO_FILTERS);
	let generation = $state(0);

	let cursor: string | null = null;
	let lastId: string | undefined;
	let ctl: AbortController | undefined;
	let queue: LogLine[] = [];
	let frame = false;
	let failures = 0;
	let retryMs = BACKOFF_BASE_MS;
	let retryTimer: ReturnType<typeof setTimeout> | undefined;
	let hiddenTimer: ReturnType<typeof setTimeout> | undefined;
	let suspended = false;
	let running = false;
	let run = 0;

	function append(batch: LogLine[]) {
		if (!batch.length) return;
		if (paused) {
			held = held.concat(batch);
			return;
		}
		const next = lines.concat(batch);
		if (next.length > MAX_LINES) {
			trimmed += next.length - MAX_LINES;
			lines = next.slice(next.length - MAX_LINES);
		} else lines = next;
		if (!following) unseen += batch.length;
	}

	function flush() {
		frame = false;
		const batch = queue;
		queue = [];
		append(batch);
	}

	function enqueue(line: LogLine) {
		if (line.id && line.id === lastId) return;
		if (line.id) lastId = line.id;
		queue.push(line);
		if (!frame) {
			frame = true;
			raf(flush);
		}
	}

	function applyPurge(kinds: string[], through: string) {
		flush();
		lines = purge(lines, kinds, through);
		held = purge(held, kinds, through);
	}

	function close() {
		ctl?.abort();
		ctl = undefined;
		clearTimeout(retryTimer);
		retryTimer = undefined;
	}

	/** Reads the newest page and starts the stream from its newest id. */
	async function open() {
		const mine = ++run;
		close();
		const c = (ctl = new AbortController());
		queue = [];
		lines = held = [];
		unseen = trimmed = skipped = 0;
		lastId = undefined;
		cursor = null;
		hasEarlier = false;
		loaded = false;
		loadError = undefined;
		status = 'connecting';
		generation++;
		try {
			const page = await api.get('/logs', {
				query: { ...query(filters), limit: PAGE },
				signal: c.signal
			});
			if (mine !== run) return;
			const chrono = page.lines.toReversed();
			lines = chrono;
			cursor = page.next_cursor;
			hasEarlier = page.more && page.next_cursor !== null;
			lastId = chrono.findLast((l) => l.id)?.id ?? undefined;
			loaded = true;
		} catch (e) {
			if (mine !== run || isAbort(e)) return;
			loadError = e instanceof ApiError ? e : new ApiError(0, 'network', 'network error');
			loaded = true;
			// The stream below may still work (history is the file/ring read, the stream is the ring).
		}
		if (mine === run && !suspended) void connect(mine);
	}

	function backoff(min = 0) {
		const exp = Math.min(BACKOFF_CAP_MS, retryMs * 2 ** Math.min(failures, 16));
		failures++;
		return Math.max(min, Math.round(exp * (0.5 + random() / 2)));
	}

	function later(mine: number, wait: number, next: StreamStatus) {
		status = next;
		clearTimeout(retryTimer);
		retryTimer = setTimeout(() => {
			retryTimer = undefined;
			if (mine === run && !suspended) void connect(mine);
		}, wait);
	}

	async function connect(mine: number) {
		if (mine !== run || suspended) return;
		ctl?.abort();
		const c = (ctl = new AbortController());
		let timedOut = false;
		status = failures ? 'reconnecting' : 'connecting';
		const headers: Record<string, string> = { Accept: 'text/event-stream' };
		if (lastId) headers['Last-Event-ID'] = lastId;
		const q = new SvelteURLSearchParams();
		for (const [k, v] of Object.entries(query(filters))) if (v !== undefined) q.set(k, String(v));
		const connectTimer = setTimeout(() => {
			timedOut = true;
			c.abort();
		}, CONNECT_TIMEOUT_MS);
		try {
			const res = await fetch(`${URL_PATH}${q.size ? `?${q}` : ''}`, {
				headers,
				credentials: 'same-origin',
				signal: c.signal
			});
			clearTimeout(connectTimer);
			if (!res.ok || !res.body) {
				const e = await errorFromResponse(res);
				if (c.signal.aborted || mine !== run) return;
				if (e.status === 401) {
					handleUnauthorized(e, '/logs/stream');
					status = 'offline';
				} else if (e.status === 429 || e.status === 503) {
					later(mine, backoff(Math.max(1, e.retryAfter ?? 0) * 1000), 'unavailable');
				} else if (e.status >= 400 && e.status < 500 && e.status !== 408) {
					status = 'unavailable'; // a bad filter or the like: retrying cannot help
				} else later(mine, backoff(), 'reconnecting');
				return;
			}
			await read(res.body, c, mine, () => {
				timedOut = true;
				c.abort();
			});
			if (c.signal.aborted && !timedOut) return;
			if (mine === run) later(mine, backoff(), 'reconnecting');
		} catch (e) {
			clearTimeout(connectTimer);
			if (!timedOut && (c.signal.aborted || isAbort(e))) return;
			if (mine === run) later(mine, backoff(), navigator.onLine ? 'reconnecting' : 'offline');
		}
	}

	async function read(
		body: ReadableStream<Uint8Array>,
		c: AbortController,
		mine: number,
		onSilence: () => void
	) {
		const reader = body.getReader();
		const decoder = new TextDecoder();
		const parser = new SseParser();
		let silence: ReturnType<typeof setTimeout> | undefined;
		const arm = () => {
			clearTimeout(silence);
			silence = setTimeout(onSilence, SILENCE_MS);
		};
		c.signal.addEventListener(
			'abort',
			() => {
				clearTimeout(silence);
				void reader.cancel().catch(() => {});
			},
			{ once: true }
		);
		arm();
		let got = false;
		try {
			for (;;) {
				const { done, value } = await reader.read();
				if (done || c.signal.aborted || mine !== run) return;
				arm();
				if (!got) {
					got = true;
					failures = 0;
					status = 'live';
				}
				for (const item of parser.push(decoder.decode(value, { stream: true }))) {
					if (item.type === 'retry') {
						retryMs = Math.max(BACKOFF_BASE_MS, item.ms);
						continue;
					}
					handle(item.event, item.data, item.id);
				}
			}
		} finally {
			clearTimeout(silence);
		}
	}

	function handle(event: string, data: string, id: string | undefined) {
		let payload: unknown;
		try {
			payload = JSON.parse(data);
		} catch {
			return; // a malformed frame is skipped, the stream carries on
		}
		if (!payload || typeof payload !== 'object') return;
		if (event === 'line') {
			const line = payload as LogLine;
			if (typeof line.ts !== 'string' || !Array.isArray(line.fields)) return;
			if (id && !line.id) line.id = id;
			enqueue(line);
		} else if (event === 'purge') {
			const p = payload as { kinds?: string[]; through?: string };
			if (Array.isArray(p.kinds) && typeof p.through === 'string') applyPurge(p.kinds, p.through);
		} else if (event === 'resync') {
			const r = payload as { skipped?: number | null };
			const n = typeof r.skipped === 'number' ? r.skipped : 0;
			// Start over from the ring; what was skipped is in history.
			void open().then(() => (skipped = n));
		}
	}

	function onVisibility() {
		clearTimeout(hiddenTimer);
		hiddenTimer = undefined;
		if (isHidden()) {
			hiddenTimer = setTimeout(() => {
				hiddenTimer = undefined;
				if (!isHidden() || !running) return;
				suspended = true;
				close();
			}, HIDDEN_CLOSE_MS);
		} else if (suspended) {
			suspended = false;
			failures = 0;
			void connect(run); // catches up from the last id
		}
	}

	function onNet() {
		if (!running) return;
		if (!navigator.onLine) status = 'offline';
		else if (!ctl && !suspended) {
			failures = 0;
			void connect(run);
		}
	}

	return {
		get lines() {
			return lines;
		},
		get status() {
			return status;
		},
		get loaded() {
			return loaded;
		},
		get loadError() {
			return loadError;
		},
		get paused() {
			return paused;
		},
		get following() {
			return following;
		},
		get unseen() {
			return unseen;
		},
		get held() {
			return held.length;
		},
		get trimmed() {
			return trimmed;
		},
		get skipped() {
			return skipped;
		},
		get hasEarlier() {
			return hasEarlier;
		},
		get loadingEarlier() {
			return loadingEarlier;
		},
		get earlierError() {
			return earlierError;
		},
		get filters() {
			return filters;
		},
		/** Changes when the list starts over (a new filter), so the view scrolls to the end. */
		get generation() {
			return generation;
		},
		start() {
			if (running) return;
			running = true;
			suspended = isHidden();
			document.addEventListener('visibilitychange', onVisibility);
			window.addEventListener('online', onNet);
			window.addEventListener('offline', onNet);
			void open();
			// A tab opened hidden loads history and takes no stream until shown.
			if (suspended) status = 'connecting';
		},
		stop() {
			running = false;
			run++;
			close();
			clearTimeout(hiddenTimer);
			document.removeEventListener('visibilitychange', onVisibility);
			window.removeEventListener('online', onNet);
			window.removeEventListener('offline', onNet);
		},
		setFilters(next: Filters) {
			filters = next;
			failures = 0;
			paused = false;
			following = true;
			if (running) void open();
		},
		setPaused(on: boolean) {
			if (on === paused) return;
			paused = on;
			if (!on && held.length) {
				const batch = held;
				held = [];
				append(batch);
			}
		},
		setFollowing(on: boolean) {
			following = on;
			if (on) unseen = 0;
		},
		async loadEarlier() {
			if (!hasEarlier || loadingEarlier || !cursor) return;
			const mine = run;
			loadingEarlier = true;
			earlierError = false;
			const room = MAX_LINES - lines.length;
			if (room <= 0) {
				hasEarlier = false;
				loadingEarlier = false;
				return;
			}
			try {
				const page = await api.get('/logs', {
					query: { ...query(filters), cursor, limit: Math.min(PAGE, room) }
				});
				if (mine !== run) return;
				lines = page.lines.toReversed().concat(lines);
				cursor = page.next_cursor;
				hasEarlier = page.more && page.next_cursor !== null && lines.length < MAX_LINES;
			} catch (e) {
				if (mine === run && !isAbort(e)) earlierError = true;
			} finally {
				if (mine === run) loadingEarlier = false;
			}
		},
		retry() {
			failures = 0;
			void (loaded && !loadError ? connect(run) : open());
		}
	};
}
export type LogView = ReturnType<typeof createLogView>;
