// The live controller (runes need a `.svelte` test file). Waits are conditions, never sleeps.
import { flushSync } from 'svelte';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from '../results/fixtures';
import type { ResultsQuery } from '../results/results.svelte';

vi.setConfig({ testTimeout: 20_000 });
const WAIT = { timeout: 10_000 };

const post = vi.hoisted(() => vi.fn());
const del = vi.hoisted(() => vi.fn());
const ev = vi.hoisted(() => ({ status: (): string => 'live' }));
const bus = vi.hoisted(() => ({
	live: undefined as undefined | ((e: { id: string; matched: number; through: number }) => void),
	resync: undefined as undefined | (() => void),
	settings: undefined as undefined | ((e: { section: string }) => void)
}));
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { post, delete: del }
}));
vi.mock('$lib/state/events.svelte', () => ({
	events: {
		get status() {
			return ev.status();
		},
		on: (topic: string, h: never, o: { onResync?: () => void } = {}) => {
			if (topic === 'search_live') {
				bus.live = h;
				bus.resync = o.onResync;
			} else bus.settings = h;
			return () => {};
		}
	}
}));
const { createLive } = await import('./live.svelte');

// Each `start()` adds window listeners: stop every one, so a test only sees its own.
const stops: (() => void)[] = [];
afterEach(() => stops.splice(0).forEach((stop) => stop()));

const q: ResultsQuery = { ast: { op: 'all' }, sort: 'relevance', hidden: false, exact: false };
const q2: ResultsQuery = {
	ast: { op: 'word', field: 'any', word: 'lantern' },
	sort: 'relevance',
	hidden: false,
	exact: false
};
const answer = (total: number, approx = false) => ({ ...page([1]), total, total_approx: approx });
const setup = (over: { total?: number; approx?: boolean; paused?: boolean } = {}) => {
	post.mockReset();
	del.mockReset();
	del.mockResolvedValue(undefined);
	const announce = vi.fn();
	const live = createLive({
		shown: () => ({ query: q, total: over.total ?? 10, approx: over.approx ?? false }),
		paused: () => over.paused ?? false,
		announce,
		intervalMs: 30_000
	});
	const start = live.start;
	live.start = () => {
		const stop = start();
		stops.push(stop);
		return stop;
	};
	return { live, announce };
};
const registers = (id = 'a1') =>
	post.mockImplementation(async (path: string) => {
		if (path === '/search/live') return { id, since: 1 };
		return answer(10);
	});
const liveCalls = () => post.mock.calls.filter(([p]) => p === '/search/live');

it('registers the query shown, and releases the previous id when it changes', async () => {
	const { live } = setup();
	live.start();
	registers('a1');
	await live.follow(q);
	expect(liveCalls()).toHaveLength(1);
	expect(liveCalls()[0]![1].body).toEqual({ ast: q.ast, q: null, hidden: false, tz: null });
	await live.follow(q); // the same query: nothing to do
	expect(liveCalls()).toHaveLength(1);
	registers('b2');
	await live.follow(q2);
	expect(del).toHaveBeenCalledWith('/search/live/{id}', { path: { id: 'a1' }, body: {} });
	expect(liveCalls()).toHaveLength(2);
	live.reset();
	await live.follow(q2, true); // Show: registers again
	expect(del).toHaveBeenCalledWith('/search/live/{id}', { path: { id: 'b2' }, body: {} });
	expect(liveCalls()).toHaveLength(3);
});

it('counts matched for the current id only, and announces once per interval', async () => {
	const { live, announce } = setup();
	live.start();
	registers('a1');
	await live.follow(q);
	bus.live!({ id: 'other', matched: 9, through: 5 });
	expect(live.fresh).toBe(0);
	bus.live!({ id: 'a1', matched: 3, through: 5 });
	expect(live.fresh).toBe(3);
	expect(announce).toHaveBeenCalledWith(3);
	bus.live!({ id: 'a1', matched: 4, through: 6 });
	expect(live.fresh).toBe(4); // cumulative, not a delta
	expect(announce).toHaveBeenCalledTimes(1);
	live.reset();
	expect(live.fresh).toBe(0);
});

it('an event for the old id after a change is ignored', async () => {
	const { live } = setup();
	live.start();
	registers('a1');
	await live.follow(q);
	registers('b2');
	await live.follow(q2);
	bus.live!({ id: 'a1', matched: 5, through: 5 });
	expect(live.fresh).toBe(0);
	bus.live!({ id: 'b2', matched: 2, through: 6 });
	expect(live.fresh).toBe(2);
});

it('holds the count while paused and shows it on the next tick once unpaused', async () => {
	vi.useFakeTimers();
	try {
		let paused = true;
		post.mockReset();
		del.mockReset();
		del.mockResolvedValue(undefined);
		const live = createLive({
			shown: () => ({ query: q, total: 10, approx: false }),
			paused: () => paused,
			intervalMs: 30_000
		});
		stops.push(live.start());
		registers('a1');
		await live.follow(q);
		bus.live!({ id: 'a1', matched: 2, through: 5 });
		expect(live.fresh).toBe(0);
		paused = false;
		await vi.advanceTimersByTimeAsync(30_000);
		expect(live.fresh).toBe(2);
	} finally {
		vi.useRealTimers();
	}
});

it('registers again after a resync or a content settings save', async () => {
	const { live } = setup();
	live.start();
	registers('a1');
	await live.follow(q);
	registers('b2');
	bus.resync!();
	await vi.waitFor(() => expect(liveCalls()).toHaveLength(2), WAIT);
	bus.settings!({ section: 'appearance' });
	expect(liveCalls()).toHaveLength(2);
	registers('c3');
	bus.settings!({ section: 'content' });
	await vi.waitFor(() => expect(liveCalls()).toHaveLength(3), WAIT);
});

it('releases its registration when stopped', async () => {
	const { live } = setup();
	const stop = live.start();
	registers('a1');
	await live.follow(q);
	stop();
	expect(del).toHaveBeenCalledWith('/search/live/{id}', { path: { id: 'a1' }, body: {} });
});

it('falls back to the probe when registration fails (429), and only then', async () => {
	vi.useFakeTimers();
	try {
		const { live, announce } = setup();
		const stop = live.start();
		post.mockImplementation(async (path: string) => {
			if (path === '/search/live') throw new Error('429');
			return answer(14);
		});
		await live.follow(q);
		await vi.advanceTimersByTimeAsync(30_000);
		expect(live.fresh).toBe(4);
		expect(announce).toHaveBeenCalledWith(4);
		expect(post.mock.calls.at(-1)![1].body).toMatchObject({ limit: 1, cursor: null });
		stop();
		// With a registration the probe never runs.
		const s = setup();
		const stop2 = s.live.start();
		registers('a1');
		await s.live.follow(q);
		post.mockClear();
		await vi.advanceTimersByTimeAsync(90_000);
		expect(post).not.toHaveBeenCalled();
		stop2();
	} finally {
		vi.useRealTimers();
	}
});

it('the probe says nothing for approximate totals, a smaller total or no change', async () => {
	vi.useFakeTimers();
	try {
		let s = setup({ approx: true });
		post.mockRejectedValue(new Error('429'));
		await s.live.follow(q);
		post.mockClear();
		await s.live.check();
		expect(post).not.toHaveBeenCalled();
		s = setup();
		post.mockRejectedValue(new Error('429'));
		await s.live.follow(q);
		for (const a of [answer(14, true), answer(10), answer(7)]) {
			post.mockResolvedValue(a);
			await s.live.check();
			expect(s.live.fresh).toBe(0);
		}
	} finally {
		vi.useRealTimers();
	}
});

it('a sort or exact change re-registers, so the count starts at 0', async () => {
	const { live } = setup();
	live.start();
	registers('a1');
	await live.follow(q);
	bus.live!({ id: 'a1', matched: 3, through: 5 });
	expect(live.fresh).toBe(3);
	registers('b2');
	await live.follow({ ...q, sort: 'seeders' });
	expect(liveCalls()).toHaveLength(2);
	expect(live.fresh).toBe(0);
	bus.live!({ id: 'b2', matched: 1, through: 6 });
	expect(live.fresh).toBe(1);
	await live.follow(null); // nothing shown: released
	expect(del).toHaveBeenCalledWith('/search/live/{id}', { path: { id: 'b2' }, body: {} });
});

it('registers again on return from a long hide, not a short one', async () => {
	vi.useFakeTimers();
	try {
		const { live } = setup();
		live.start();
		registers('a1');
		await live.follow(q);
		const hidden = vi.spyOn(document, 'hidden', 'get');
		const flip = (isHidden: boolean) => {
			hidden.mockReturnValue(isHidden);
			document.dispatchEvent(new Event('visibilitychange'));
		};
		flip(true);
		vi.advanceTimersByTime(5_000);
		flip(false);
		expect(liveCalls()).toHaveLength(1);
		flip(true);
		vi.advanceTimersByTime(26_000);
		registers('b2');
		flip(false);
		await vi.waitFor(() => expect(liveCalls()).toHaveLength(2), WAIT);
		hidden.mockRestore();
	} finally {
		vi.useRealTimers();
	}
});

it('registers again when restored from the back/forward cache', async () => {
	const { live } = setup();
	live.start();
	registers('a1');
	await live.follow(q);
	registers('b2');
	window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: false }));
	expect(liveCalls()).toHaveLength(1);
	window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
	await vi.waitFor(() => expect(liveCalls()).toHaveLength(2), WAIT);
});

it('announces a count that changed inside the throttle window on a later tick', async () => {
	vi.useFakeTimers();
	try {
		const { live, announce } = setup();
		live.start();
		registers('a1');
		await live.follow(q);
		bus.live!({ id: 'a1', matched: 3, through: 5 });
		bus.live!({ id: 'a1', matched: 4, through: 6 });
		expect(announce).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(30_000);
		expect(announce).toHaveBeenLastCalledWith(4);
	} finally {
		vi.useRealTimers();
	}
});

let statusNow = $state('live');
let totalNow = $state(10);
it('coalesces the visibility and status re-registrations, and the status effect does not track search state', async () => {
	vi.useFakeTimers();
	let reads = 0;
	ev.status = () => {
		reads++;
		return statusNow;
	};
	try {
		post.mockReset();
		del.mockReset();
		del.mockResolvedValue(undefined);
		registers('a1');
		const live = createLive({
			shown: () => ({ query: q, total: totalNow, approx: false }),
			paused: () => false,
			intervalMs: 30_000
		});
		stops.push(live.start());
		await live.follow(q);
		// The tab is hidden and the stream is down for the same 30 s: both paths fire on return.
		const hidden = vi.spyOn(document, 'hidden', 'get').mockReturnValue(true);
		document.dispatchEvent(new Event('visibilitychange'));
		statusNow = 'down';
		flushSync();
		await vi.advanceTimersByTimeAsync(30_000);
		hidden.mockReturnValue(false);
		document.dispatchEvent(new Event('visibilitychange'));
		statusNow = 'live';
		flushSync();
		await vi.advanceTimersByTimeAsync(0);
		expect(liveCalls()).toHaveLength(2); // one re-registration, not two
		hidden.mockRestore();
		// `again()` read the shown query inside the effect: a search-state change must not re-run it.
		const before = reads;
		totalNow = 11;
		flushSync();
		expect(reads).toBe(before);
	} finally {
		ev.status = () => 'live';
		statusNow = 'live';
		vi.useRealTimers();
	}
});
