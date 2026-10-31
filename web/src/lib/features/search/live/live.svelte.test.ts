// The live controller (runes need a `.svelte` test file). Waits are conditions, never sleeps.
import { expect, it, vi } from 'vitest';
import { page } from '../results/fixtures';
import type { ResultsQuery } from '../results/results.svelte';

vi.setConfig({ testTimeout: 20_000 });

const post = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { post }
}));
const { createLive } = await import('./live.svelte');

const q: ResultsQuery = { ast: { op: 'all' }, sort: 'relevance', hidden: false, exact: false };
const answer = (total: number, approx = false) => ({
	...page([1]),
	total,
	total_approx: approx
});
const setup = (over: { total?: number; approx?: boolean; paused?: boolean } = {}) => {
	post.mockReset();
	const announce = vi.fn();
	const live = createLive({
		shown: () => ({ query: q, total: over.total ?? 10, approx: over.approx ?? false }),
		paused: () => over.paused ?? false,
		announce,
		intervalMs: 30_000
	});
	return { live, announce };
};

it('counts new results from exact totals, with limit 1, and announces once', async () => {
	const { live, announce } = setup();
	post.mockResolvedValue(answer(14));
	await live.check();
	expect(live.fresh).toBe(4);
	expect(post.mock.calls[0]![1].body).toMatchObject({ limit: 1, cursor: null, sort: 'relevance' });
	expect(announce).toHaveBeenCalledWith(4);
	post.mockResolvedValue(answer(16));
	await live.check();
	expect(live.fresh).toBe(6);
	expect(announce).toHaveBeenCalledTimes(1); // throttled: the next one waits for the interval
});

it('says nothing for approximate totals, a smaller total or no change', async () => {
	let s = setup({ approx: true });
	await s.live.check();
	expect(post).not.toHaveBeenCalled();
	s = setup();
	post.mockResolvedValue(answer(14, true));
	await s.live.check();
	expect(s.live.fresh).toBe(0);
	post.mockResolvedValue(answer(10));
	await s.live.check();
	expect(s.live.fresh).toBe(0);
	post.mockResolvedValue(answer(7));
	await s.live.check();
	expect(s.live.fresh).toBe(0);
});

it('sends nothing while paused, hidden, or with a request pending', async () => {
	let s = setup({ paused: true });
	await s.live.check();
	expect(post).not.toHaveBeenCalled();

	s = setup();
	const hidden = vi.spyOn(document, 'hidden', 'get').mockReturnValue(true);
	await s.live.check();
	expect(post).not.toHaveBeenCalled();
	hidden.mockRestore();

	let release!: (v: unknown) => void;
	post.mockReturnValue(new Promise((r) => (release = r)));
	const first = s.live.check();
	await s.live.check();
	expect(post).toHaveBeenCalledTimes(1);
	release(answer(12));
	await first;
	expect(s.live.fresh).toBe(2);
});

it('reset clears the count, and a failed check says nothing', async () => {
	const { live } = setup();
	post.mockResolvedValue(answer(12));
	await live.check();
	expect(live.fresh).toBe(2);
	live.reset();
	expect(live.fresh).toBe(0);
	post.mockRejectedValue(new Error('down'));
	await live.check();
	expect(live.fresh).toBe(0);
});

it('polls on the interval until stopped', async () => {
	vi.useFakeTimers();
	try {
		const { live } = setup();
		post.mockResolvedValue(answer(11));
		const stop = live.start();
		await vi.advanceTimersByTimeAsync(29_999);
		expect(post).not.toHaveBeenCalled();
		await vi.advanceTimersByTimeAsync(1);
		expect(post).toHaveBeenCalledTimes(1);
		stop();
		await vi.advanceTimersByTimeAsync(60_000);
		expect(post).toHaveBeenCalledTimes(1);
	} finally {
		vi.useRealTimers();
	}
});
