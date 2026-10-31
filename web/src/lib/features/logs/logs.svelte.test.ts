import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync } from 'svelte';
import type { LogLine } from './text';

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { get }
}));

import { createLogView, MAX_LINES, HIDDEN_CLOSE_MS } from './logs.svelte';

const line = (n: number, over: Partial<LogLine> = {}): LogLine => ({
	component: 'dht',
	event: null,
	fields: [],
	id: `aa-${n}`,
	level: 'info',
	msg: `line ${n}`,
	opt_in: null,
	repeated: null,
	target: null,
	truncated: false,
	ts: '2026-10-31T10:00:00.000Z',
	...over
});

/** A stream the test writes frames to. */
function streams() {
	const opened: {
		url: string;
		headers: Record<string, string>;
		push: (s: string) => void;
		end: () => void;
		signal: AbortSignal;
	}[] = [];
	vi.stubGlobal(
		'fetch',
		vi.fn((url: string, init: RequestInit) => {
			let ctrl!: ReadableStreamDefaultController<Uint8Array>;
			const body = new ReadableStream<Uint8Array>({ start: (c) => (ctrl = c) });
			const enc = new TextEncoder();
			const signal = init.signal as AbortSignal;
			signal.addEventListener('abort', () => {
				try {
					ctrl.close();
				} catch {
					/* closed */
				}
			});
			opened.push({
				url,
				headers: init.headers as Record<string, string>,
				signal,
				push: (s) => ctrl.enqueue(enc.encode(s)),
				end: () => ctrl.close()
			});
			return Promise.resolve(new Response(body, { status: 200 }));
		})
	);
	return opened;
}
const frame = (l: LogLine) => `id: ${l.id}\nevent: line\ndata: ${JSON.stringify(l)}\n\n`;

let rafs: (() => void)[] = [];
const frames = () => {
	const run = rafs;
	rafs = [];
	run.forEach((f) => f());
	flushSync();
};
const settle = async () => {
	for (let i = 0; i < 20; i++) await Promise.resolve();
};
const make = () =>
	createLogView({ raf: (cb) => rafs.push(cb), random: () => 1, hidden: () => hiddenNow });
let hiddenNow = false;

beforeEach(() => {
	rafs = [];
	hiddenNow = false;
	get.mockReset();
	get.mockResolvedValue({
		lines: [line(2), line(1)],
		more: true,
		next_cursor: 'c1',
		unreadable: 0
	});
});
afterEach(() => {
	vi.unstubAllGlobals();
	vi.useRealTimers();
});

describe('opening', () => {
	it('shows history oldest first, then streams from its newest id with the filters', async () => {
		const opened = streams();
		const v = make();
		v.setFilters({ level: 'warn', components: ['dht', 'api'], text: ' hello ' });
		v.start();
		await settle();
		expect(get).toHaveBeenLastCalledWith(
			'/logs',
			expect.objectContaining({
				query: expect.objectContaining({
					level: 'warn',
					components: 'dht,api',
					text: 'hello',
					limit: 500
				})
			})
		);
		expect(v.lines.map((l) => l.id)).toEqual(['aa-1', 'aa-2']);
		expect(opened).toHaveLength(1);
		expect(opened[0]!.headers['Last-Event-ID']).toBe('aa-2');
		const q = new URL(opened[0]!.url, 'http://x').searchParams;
		expect([q.get('level'), q.get('components'), q.get('text')]).toEqual([
			'warn',
			'dht,api',
			'hello'
		]);
		v.stop();
	});
});

describe('batching, cap, follow and pause', () => {
	it('applies all lines that arrive in one frame together', async () => {
		const opened = streams();
		const v = make();
		v.start();
		await settle();
		opened[0]!.push(frame(line(3)) + frame(line(4)) + frame(line(5)));
		await settle();
		expect(v.lines).toHaveLength(2); // nothing before the frame
		frames();
		expect(v.lines.map((l) => l.id)).toEqual(['aa-1', 'aa-2', 'aa-3', 'aa-4', 'aa-5']);
		v.stop();
	});

	it('keeps at most 10,000 lines and counts what it dropped', async () => {
		const opened = streams();
		get.mockResolvedValue({ lines: [], more: false, next_cursor: null, unreadable: 0 });
		const v = make();
		v.start();
		await settle();
		const n = MAX_LINES + 25;
		opened[0]!.push(Array.from({ length: n }, (_, i) => frame(line(i + 1))).join(''));
		await settle();
		frames();
		expect(v.lines).toHaveLength(MAX_LINES);
		expect(v.trimmed).toBe(25);
		expect(v.lines[0]!.id).toBe('aa-26');
		v.stop();
	});

	it('counts unseen lines while not following, and clears them on following again', async () => {
		const opened = streams();
		const v = make();
		v.start();
		await settle();
		v.setFollowing(false);
		opened[0]!.push(frame(line(3)) + frame(line(4)));
		await settle();
		frames();
		expect(v.unseen).toBe(2);
		v.setFollowing(true);
		expect(v.unseen).toBe(0);
		v.stop();
	});

	it('freezes the view on pause, holds arriving lines, and shows them on resume', async () => {
		const opened = streams();
		const v = make();
		v.start();
		await settle();
		v.setPaused(true);
		opened[0]!.push(frame(line(3)));
		await settle();
		frames();
		expect(v.lines).toHaveLength(2);
		expect(v.held).toBe(1);
		v.setPaused(false);
		expect(v.lines.map((l) => l.id)).toEqual(['aa-1', 'aa-2', 'aa-3']);
		expect(v.held).toBe(0);
		v.stop();
	});
});

describe('the hidden tab', () => {
	it('closes the stream after a while hidden and resumes from the last id on return', async () => {
		vi.useFakeTimers();
		const opened = streams();
		const v = make();
		v.start();
		await settle();
		opened[0]!.push(frame(line(3)));
		await settle();
		frames();
		hiddenNow = true;
		document.dispatchEvent(new Event('visibilitychange'));
		await vi.advanceTimersByTimeAsync(HIDDEN_CLOSE_MS + 1);
		expect(opened[0]!.signal.aborted).toBe(true);
		expect(opened).toHaveLength(1);
		hiddenNow = false;
		document.dispatchEvent(new Event('visibilitychange'));
		await settle();
		expect(opened).toHaveLength(2);
		expect(opened[1]!.headers['Last-Event-ID']).toBe('aa-3');
		v.stop();
	});
});

describe('reconnecting', () => {
	it('backs off between attempts and resumes from the last id', async () => {
		vi.useFakeTimers();
		const opened = streams();
		const v = make();
		v.start();
		await settle();
		opened[0]!.push(frame(line(3)));
		await settle();
		opened[0]!.end();
		await settle();
		expect(v.status).toBe('reconnecting');
		expect(opened).toHaveLength(1); // not at once
		await vi.advanceTimersByTimeAsync(1_500);
		expect(opened).toHaveLength(2);
		expect(opened[1]!.headers['Last-Event-ID']).toBe('aa-3');
		v.stop();
	});
});

describe('purge and paging', () => {
	it('hides the kinds a purge names and drops an opt-in’s lines', async () => {
		const opened = streams();
		get.mockResolvedValue({
			lines: [
				line(2, { opt_in: 'peer_activity' }),
				line(1, { fields: [{ kind: 'hash', name: 'h', value: 'abcd' }] })
			],
			more: false,
			next_cursor: null,
			unreadable: 0
		});
		const v = make();
		v.start();
		await settle();
		opened[0]!.push('event: purge\ndata: {"kinds":["hash","peer_activity"],"through":"aa-2"}\n\n');
		await settle();
		expect(v.lines.map((l) => l.id)).toEqual(['aa-1']);
		expect(v.lines[0]!.fields[0]!.value).toBe('[hidden]');
		v.stop();
	});

	it('pages earlier lines in front, and stops at the cap', async () => {
		streams();
		const v = make();
		v.start();
		await settle();
		get.mockResolvedValueOnce({ lines: [line(0)], more: false, next_cursor: null, unreadable: 0 });
		await v.loadEarlier();
		expect(get).toHaveBeenLastCalledWith(
			'/logs',
			expect.objectContaining({ query: expect.objectContaining({ cursor: 'c1' }) })
		);
		expect(v.lines.map((l) => l.id)).toEqual(['aa-0', 'aa-1', 'aa-2']);
		expect(v.hasEarlier).toBe(false);
		v.stop();
	});
});
