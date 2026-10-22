import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { onLoginRequired } from '$lib/api/client';
import {
	BACKOFF_CAP_MS,
	HEARTBEAT_TIMEOUT_MS,
	HIDDEN_CLOSE_MS,
	createEvents
} from './events.svelte';

const enc = new TextEncoder();

function stream() {
	let ctrl!: ReadableStreamDefaultController<Uint8Array>;
	const body = new ReadableStream<Uint8Array>({ start: (c) => void (ctrl = c) });
	return {
		response: () => new Response(body, { status: 200 }),
		push: (s: string) => ctrl.enqueue(enc.encode(s)),
		end: () => ctrl.close()
	};
}
const frame = (event: string, data: unknown, id?: string) =>
	`${id ? `id: ${id}\n` : ''}event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
const err = (status: number, code: string, headers: Record<string, string> = {}) =>
	new Response(JSON.stringify({ error: { code, message: code } }), { status, headers });

let fetchMock: ReturnType<typeof vi.fn>;
let frames: (() => void)[];
let visibility: 'visible' | 'hidden';
const setVisibility = (v: 'visible' | 'hidden') => {
	visibility = v;
	document.dispatchEvent(new Event('visibilitychange'));
};
const flushFrame = () => frames.splice(0).forEach((f) => f());
const tick = (ms = 0) => vi.advanceTimersByTimeAsync(ms);

const offs: (() => void)[] = [];
function make() {
	const ev = createEvents({ random: () => 1, raf: (cb) => frames.push(cb) });
	const on = ev.on;
	// Tests end with consumers still subscribed; release them so no listener leaks across tests.
	ev.on = ((...args: Parameters<typeof on>) => {
		const off = on(...args);
		offs.push(off);
		return off;
	}) as typeof on;
	return ev;
}
const headersOf = (n: number) => fetchMock.mock.calls[n]![1].headers as Record<string, string>;
const urlOf = (n: number) => fetchMock.mock.calls[n]![0] as string;

beforeEach(() => {
	vi.useFakeTimers();
	frames = [];
	visibility = 'visible';
	Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => visibility });
	fetchMock = vi.fn();
	vi.stubGlobal('fetch', fetchMock);
	onLoginRequired(undefined);
});
afterEach(() => {
	offs.splice(0).forEach((off) => off());
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

describe('connection', () => {
	it('opens one stream for the union of topics and none without consumers', async () => {
		const ev = make();
		expect(ev.status).toBe('stopped');
		const s = stream();
		fetchMock.mockResolvedValue(s.response());
		const off1 = ev.on('torrents', () => {});
		const off2 = ev.on('crawler', () => {});
		await tick();
		expect(fetchMock).toHaveBeenCalledTimes(1);
		expect(urlOf(0)).toBe('/api/v1/events?topics=crawler%2Ctorrents');
		expect(ev.status).toBe('live');
		off1();
		off2();
		await tick();
		expect(ev.status).toBe('stopped');
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});

	it('reconnects with the new topic set and Last-Event-ID when topics change', async () => {
		const ev = make();
		const a = stream();
		const b = stream();
		fetchMock.mockResolvedValueOnce(a.response()).mockResolvedValueOnce(b.response());
		ev.on('torrents', () => {});
		await tick();
		a.push(frame('torrents', { added: 1, max_id: 1 }, 'e-1'));
		await tick();
		ev.on('rebuild', () => {});
		await tick();
		expect(fetchMock).toHaveBeenCalledTimes(2);
		expect(urlOf(1)).toBe('/api/v1/events?topics=rebuild%2Ctorrents');
		expect(headersOf(1)['Last-Event-ID']).toBe('e-1');
		expect(headersOf(0)['Last-Event-ID']).toBeUndefined();
	});

	it('does not reconnect when a second consumer wants a topic already open', async () => {
		const ev = make();
		fetchMock.mockResolvedValue(stream().response());
		ev.on('torrents', () => {});
		ev.on('torrents', () => {});
		await tick();
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});
});

describe('reconnect', () => {
	it('reconnects after a clean close, after the retry: delay, with Last-Event-ID', async () => {
		const ev = make();
		const a = stream();
		const b = stream();
		fetchMock.mockResolvedValueOnce(a.response()).mockResolvedValueOnce(b.response());
		ev.on('torrents', () => {});
		await tick();
		a.push('retry: 2000\n\n' + frame('torrents', { added: 1, max_id: 1 }, 'e-7'));
		await tick();
		a.end();
		await tick();
		expect(ev.status).toBe('offline');
		await tick(1999);
		expect(fetchMock).toHaveBeenCalledTimes(1);
		await tick(1);
		expect(fetchMock).toHaveBeenCalledTimes(2);
		expect(headersOf(1)['Last-Event-ID']).toBe('e-7');
		expect(ev.status).toBe('live');
	});

	it('backs off exponentially with jitter and caps the delay', async () => {
		const ev = make();
		fetchMock.mockImplementation(async () => err(503, 'events_unavailable'));
		ev.on('torrents', () => {});
		await tick();
		const gaps: number[] = [];
		let calls = fetchMock.mock.calls.length;
		for (let i = 0; i < 8; i++) {
			let waited = 0;
			while (fetchMock.mock.calls.length === calls) {
				await tick(500);
				waited += 500;
			}
			calls = fetchMock.mock.calls.length;
			gaps.push(waited);
		}
		// base 5 s: 5, 10, 20, then capped at 30 s
		expect(gaps.slice(0, 3)).toEqual([5000, 10000, 20000]);
		expect(Math.max(...gaps)).toBe(BACKOFF_CAP_MS);
		expect(gaps.slice(-2)).toEqual([BACKOFF_CAP_MS, BACKOFF_CAP_MS]);
		expect(ev.status).toBe('offline');
	});

	it('reconnects when the stream goes silent for the heartbeat timeout', async () => {
		const ev = make();
		fetchMock.mockImplementation(async () => stream().response());
		ev.on('torrents', () => {});
		await tick();
		await tick(HEARTBEAT_TIMEOUT_MS - 1);
		expect(ev.status).toBe('live');
		await tick(1);
		expect(ev.status).toBe('offline');
		await tick(5000);
		expect(fetchMock).toHaveBeenCalledTimes(2);
	});

	it('a heartbeat comment keeps the stream alive', async () => {
		const ev = make();
		const s = stream();
		fetchMock.mockResolvedValue(s.response());
		ev.on('torrents', () => {});
		await tick();
		for (let i = 0; i < 4; i++) {
			await tick(15_000);
			s.push(': hb\n\n');
		}
		expect(ev.status).toBe('live');
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});

	it('reconnects after a network error', async () => {
		const ev = make();
		fetchMock
			.mockRejectedValueOnce(new TypeError('offline'))
			.mockResolvedValue(stream().response());
		ev.on('torrents', () => {});
		await tick();
		expect(ev.status).toBe('offline');
		expect(ev.error?.code).toBe('network');
		await tick(5000);
		expect(ev.status).toBe('live');
		expect(ev.error).toBeUndefined();
	});
});

describe('jitter', () => {
	it('waits 50-100 % of the backoff', async () => {
		const ev = createEvents({ random: () => 0, raf: (cb) => frames.push(cb) });
		const off = ev.on;
		fetchMock.mockImplementation(async () => err(503, 'events_unavailable'));
		offs.push(off('torrents', () => {}));
		await tick();
		await tick(2499);
		expect(fetchMock).toHaveBeenCalledTimes(1);
		await tick(1);
		expect(fetchMock).toHaveBeenCalledTimes(2);
	});
});

describe('errors', () => {
	it('401 calls the login handler and does not retry', async () => {
		const handler = vi.fn();
		onLoginRequired(handler);
		const ev = make();
		fetchMock.mockImplementation(async () => err(401, 'login_required'));
		ev.on('torrents', () => {});
		await tick();
		expect(handler).toHaveBeenCalledTimes(1);
		expect(ev.status).toBe('stopped');
		expect(ev.error?.code).toBe('login_required');
		await tick(120_000);
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});

	it('429 shows busy, waits Retry-After, then retries', async () => {
		const ev = make();
		fetchMock
			.mockResolvedValueOnce(err(429, 'too_many_streams', { 'Retry-After': '12' }))
			.mockResolvedValueOnce(stream().response());
		ev.on('torrents', () => {});
		await tick();
		expect(ev.status).toBe('busy');
		expect(ev.error?.retryAfter).toBe(12);
		await tick(11_999);
		expect(fetchMock).toHaveBeenCalledTimes(1);
		await tick(1);
		expect(fetchMock).toHaveBeenCalledTimes(2);
		expect(ev.status).toBe('live');
	});

	it('a topic change while busy does not hammer the server', async () => {
		const ev = make();
		fetchMock
			.mockResolvedValueOnce(err(429, 'too_many_streams', { 'Retry-After': '10' }))
			.mockResolvedValueOnce(stream().response());
		ev.on('torrents', () => {});
		await tick();
		ev.on('crawler', () => {});
		await tick(5000);
		expect(fetchMock).toHaveBeenCalledTimes(1);
		await tick(5000);
		expect(fetchMock).toHaveBeenCalledTimes(2);
		expect(urlOf(1)).toContain('crawler');
	});

	it('an unknown-topic 400 stops without retrying', async () => {
		const ev = make();
		fetchMock.mockImplementation(async () => err(400, 'unknown_topic'));
		ev.on('torrents', () => {});
		await tick(60_000);
		expect(fetchMock).toHaveBeenCalledTimes(1);
		expect(ev.status).toBe('stopped');
	});
});

describe('delivery', () => {
	it('batches per frame in arrival order and routes by topic', async () => {
		const ev = make();
		const s = stream();
		fetchMock.mockResolvedValue(s.response());
		const log: string[] = [];
		ev.on('torrents', (d) => log.push(`t${d.added}`));
		ev.on('rebuild', () => log.push('r'));
		await tick();
		s.push(frame('torrents', { added: 1, max_id: 1 }, 'a-1'));
		s.push(frame('rebuild', {}, 'a-2'));
		s.push(frame('torrents', { added: 2, max_id: 3 }, 'a-3'));
		await tick();
		expect(log).toEqual([]);
		expect(frames).toHaveLength(1);
		flushFrame();
		expect(log).toEqual(['t1', 'r', 't2']);
	});

	it('delivers only the latest crawler snapshot of a frame, but every torrents event', async () => {
		const ev = make();
		const s = stream();
		fetchMock.mockResolvedValue(s.response());
		const log: string[] = [];
		ev.on('crawler', (d) => log.push(`c${d.torrents}`));
		ev.on('torrents', (d) => log.push(`t${d.added}`));
		await tick();
		s.push(frame('crawler', { torrents: 1 }, 'a-1'));
		s.push(frame('torrents', { added: 5, max_id: 1 }, 'a-2'));
		s.push(frame('crawler', { torrents: 2 }, 'a-3'));
		s.push(frame('torrents', { added: 6, max_id: 2 }, 'a-4'));
		s.push(frame('crawler', { torrents: 3 }, 'a-5'));
		await tick();
		flushFrame();
		expect(log).toEqual(['t5', 't6', 'c3']);
		s.push(frame('crawler', { torrents: 4 }, 'a-6'));
		await tick();
		flushFrame();
		expect(log.at(-1)).toBe('c4');
	});

	it('resync fans out to every consumer in order and the stream carries on', async () => {
		const ev = make();
		const s = stream();
		fetchMock.mockResolvedValue(s.response());
		const log: string[] = [];
		ev.on('torrents', (d) => log.push(`t${d.added}`), { onResync: () => log.push('r1') });
		ev.on('crawler', () => {}, { onResync: () => log.push('r2') });
		ev.on('rebuild', () => {});
		await tick();
		s.push(frame('resync', { reason: 'lagged' }));
		s.push(frame('torrents', { added: 1, max_id: 1 }, 'a-1'));
		await tick();
		flushFrame();
		expect(log).toEqual(['r1', 'r2', 't1']);
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});

	it('a resync has no id, so Last-Event-ID stays on the last real event', async () => {
		const ev = make();
		const a = stream();
		fetchMock.mockResolvedValueOnce(a.response()).mockResolvedValue(stream().response());
		ev.on('torrents', () => {});
		await tick();
		a.push(
			frame('torrents', { added: 1, max_id: 1 }, 'a-4') + frame('resync', { reason: 'lagged' })
		);
		await tick();
		a.end();
		await tick(5000);
		expect(headersOf(1)['Last-Event-ID']).toBe('a-4');
	});

	it('a throwing handler and a malformed payload do not break the others', async () => {
		vi.stubGlobal('reportError', vi.fn());
		const ev = make();
		const s = stream();
		fetchMock.mockResolvedValue(s.response());
		const ok = vi.fn();
		ev.on('torrents', () => {
			throw new Error('boom');
		});
		ev.on('torrents', ok);
		await tick();
		s.push('event: torrents\ndata: {not json\n\n');
		s.push(frame('torrents', { added: 1, max_id: 1 }, 'a-1'));
		await tick();
		flushFrame();
		expect(ok).toHaveBeenCalledTimes(1);
		expect(globalThis.reportError).toHaveBeenCalledTimes(1);
	});

	it('stops delivering to an unsubscribed consumer', async () => {
		const ev = make();
		const s = stream();
		fetchMock.mockResolvedValue(s.response());
		const a = vi.fn();
		const b = vi.fn();
		const off = ev.on('torrents', a);
		ev.on('torrents', b);
		await tick();
		s.push(frame('torrents', { added: 1, max_id: 1 }, 'a-1'));
		await tick();
		off();
		flushFrame();
		expect(a).not.toHaveBeenCalled();
		expect(b).toHaveBeenCalledTimes(1);
	});
});

describe('hidden tab', () => {
	it('closes the stream after 10 s hidden and reopens with Last-Event-ID when visible', async () => {
		const ev = make();
		const a = stream();
		fetchMock.mockResolvedValueOnce(a.response()).mockResolvedValue(stream().response());
		ev.on('torrents', () => {});
		await tick();
		a.push(frame('torrents', { added: 1, max_id: 1 }, 'a-9'));
		await tick();
		setVisibility('hidden');
		await tick(HIDDEN_CLOSE_MS - 1);
		expect(ev.status).toBe('live');
		await tick(1);
		expect(ev.status).toBe('stopped');
		await tick(120_000);
		expect(fetchMock).toHaveBeenCalledTimes(1);
		setVisibility('visible');
		await tick();
		expect(fetchMock).toHaveBeenCalledTimes(2);
		expect(headersOf(1)['Last-Event-ID']).toBe('a-9');
		expect(ev.status).toBe('live');
	});

	it('keeps the stream when the tab is visible again within 10 s', async () => {
		const ev = make();
		fetchMock.mockImplementation(async () => stream().response());
		ev.on('torrents', () => {});
		await tick();
		setVisibility('hidden');
		await tick(HIDDEN_CLOSE_MS - 1000);
		setVisibility('visible');
		await tick(HIDDEN_CLOSE_MS * 2);
		expect(fetchMock).toHaveBeenCalledTimes(1);
		expect(ev.status).toBe('live');
	});

	it('does not reopen for a consumer added while suspended until visible', async () => {
		const ev = make();
		fetchMock.mockImplementation(async () => stream().response());
		const off = ev.on('torrents', () => {});
		await tick();
		setVisibility('hidden');
		await tick(HIDDEN_CLOSE_MS);
		ev.on('crawler', () => {});
		await tick();
		expect(fetchMock).toHaveBeenCalledTimes(1);
		off();
		setVisibility('visible');
		await tick();
		expect(fetchMock).toHaveBeenCalledTimes(2);
		expect(urlOf(1)).toContain('topics=crawler');
	});
});
