import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { SearchHit, TorrentDetail } from './cache';

const pushState = vi.fn();
const replaceState = vi.fn();
const get = vi.fn();
const page = {
	url: new URL('http://x/search?q=ubuntu'),
	state: {} as { detail?: { hash: string } }
};

vi.mock('$app/navigation', () => ({ pushState, replaceState }));
vi.mock('$app/state', () => ({ page }));
vi.mock('$app/paths', () => ({
	resolve: (route: string, params?: Record<string, string>) =>
		route.replace('[hash]', params?.hash ?? '')
}));
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get }
}));

const { openDetail, closeDetail, markClosed, prefetchDetail, returnFocusTarget } =
	await import('./open');
const { clearDetailCaches, cachedDetail, cachedHit, cacheDetail, FRESH_MS } =
	await import('./cache');

const H1 = 'a'.repeat(40);
const H2 = 'b'.repeat(40);
const hit = { hash: H1, name: 'n' } as SearchHit;
const detail = (hash: string) => ({ hash, name: hash.slice(0, 4) }) as TorrentDetail;
const back = vi.spyOn(history, 'back').mockImplementation(() => {});

beforeEach(() => {
	vi.useFakeTimers();
	pushState.mockReset();
	replaceState.mockReset();
	get.mockReset();
	back.mockClear();
	page.state = {};
	markClosed();
	clearDetailCaches();
});
afterEach(() => vi.useRealTimers());

describe('openDetail', () => {
	it('pushes /t/{hash} with the query string and the hash in the state', () => {
		openDetail(H1, { hit });
		expect(pushState).toHaveBeenCalledWith(`/t/${H1}?q=ubuntu`, { detail: { hash: H1 } });
		expect(replaceState).not.toHaveBeenCalled();
		expect(cachedHit(H1)).toBe(hit);
	});

	it('replaces when asked and a drawer is already open (j/k)', () => {
		page.state = { detail: { hash: H1 } };
		openDetail(H2, { replace: true });
		expect(replaceState).toHaveBeenCalledWith(`/t/${H2}?q=ubuntu`, { detail: { hash: H2 } });
		expect(pushState).not.toHaveBeenCalled();
	});

	it('replaces whenever a drawer is open, so one Back leaves it', () => {
		openDetail(H1);
		page.state = { detail: { hash: H1 } };
		openDetail(H2); // no replace flag: still one entry per drawer session
		expect(pushState).toHaveBeenCalledTimes(1);
		expect(replaceState).toHaveBeenCalledWith(`/t/${H2}?q=ubuntu`, { detail: { hash: H2 } });
		closeDetail();
		expect(back).toHaveBeenCalledTimes(1);
	});

	it('remembers where focus returns', () => {
		const el = document.createElement('button');
		openDetail(H1, { returnFocus: () => el });
		expect(returnFocusTarget()).toBe(el);
	});
});

describe('closeDetail', () => {
	it('goes Back when openDetail pushed the entry', () => {
		openDetail(H1);
		page.state = { detail: { hash: H1 } };
		closeDetail();
		expect(back).toHaveBeenCalledTimes(1);
		expect(replaceState).not.toHaveBeenCalled();
	});

	it('keeps Back for the entry through j/k replaces', () => {
		openDetail(H1);
		page.state = { detail: { hash: H1 } };
		openDetail(H2, { replace: true });
		closeDetail();
		expect(back).toHaveBeenCalledTimes(1);
	});

	it('replaces with /search when the entry was not pushed (a reload or a direct link)', () => {
		page.state = { detail: { hash: H1 } };
		closeDetail();
		expect(back).not.toHaveBeenCalled();
		expect(replaceState).toHaveBeenCalledWith('/search?q=ubuntu', {});
	});

	it('forgets the push once the drawer closed by another route', () => {
		openDetail(H1);
		markClosed();
		page.state = { detail: { hash: H2 } };
		closeDetail();
		expect(back).not.toHaveBeenCalled();
		expect(replaceState).toHaveBeenCalled();
	});
});

describe('prefetchDetail', () => {
	it('fetches after 150 ms idle into the cache', async () => {
		get.mockResolvedValue(detail(H1));
		prefetchDetail(H1);
		await vi.advanceTimersByTimeAsync(149);
		expect(get).not.toHaveBeenCalled();
		await vi.advanceTimersByTimeAsync(1);
		expect(get).toHaveBeenCalledWith('/torrents/{hash}', {
			path: { hash: H1 },
			signal: expect.any(AbortSignal)
		});
		await vi.advanceTimersByTimeAsync(0);
		expect(cachedDetail(H1)?.hash).toBe(H1);
	});

	it('a newer call cancels a pending one (debounce)', async () => {
		get.mockImplementation(async (_p, o) => detail(o.path.hash));
		prefetchDetail(H1);
		await vi.advanceTimersByTimeAsync(100);
		prefetchDetail(H2);
		await vi.advanceTimersByTimeAsync(400);
		expect(get).toHaveBeenCalledTimes(1);
		expect(get.mock.calls[0]![1].path.hash).toBe(H2);
		expect(cachedDetail(H1)).toBeUndefined();
	});

	it('a newer call aborts the request in flight and its result is dropped', async () => {
		let signal!: AbortSignal;
		get.mockImplementationOnce(
			(_p, o) =>
				new Promise((resolve) => {
					signal = o.signal;
					setTimeout(() => resolve(detail(H1)), 1000);
				})
		);
		get.mockImplementationOnce(async () => detail(H2));
		prefetchDetail(H1);
		await vi.advanceTimersByTimeAsync(150);
		prefetchDetail(H2);
		expect(signal.aborted).toBe(true);
		await vi.advanceTimersByTimeAsync(2000);
		expect(cachedDetail(H1)).toBeUndefined();
		expect(cachedDetail(H2)?.hash).toBe(H2);
	});

	it('does not refetch what is cached and fresh', async () => {
		cacheDetail(H1, detail(H1));
		prefetchDetail(H1);
		await vi.advanceTimersByTimeAsync(500);
		expect(get).not.toHaveBeenCalled();
	});

	it('swallows a failed prefetch', async () => {
		get.mockRejectedValue(new Error('offline'));
		prefetchDetail(H1);
		await vi.advanceTimersByTimeAsync(500);
		expect(cachedDetail(H1)).toBeUndefined();
	});
});

describe('the detail cache', () => {
	it('keeps the 50 most recently used', () => {
		for (let i = 0; i < 51; i++) cacheDetail(String(i).padStart(40, '0'), detail(String(i)));
		expect(cachedDetail('0'.repeat(40))).toBeUndefined();
		expect(cachedDetail(String(50).padStart(40, '0'))).toBeDefined();
		// a read refreshes: touch 1, add one more, 2 is evicted instead
		expect(cachedDetail(String(1).padStart(40, '0'))).toBeDefined();
		cacheDetail('f'.repeat(40), detail('f'));
		expect(cachedDetail(String(1).padStart(40, '0'))).toBeDefined();
		expect(cachedDetail(String(2).padStart(40, '0'))).toBeUndefined();
	});

	it('expires an entry after a minute', () => {
		cacheDetail(H1, detail(H1), 0);
		expect(cachedDetail(H1, FRESH_MS)).toBeDefined();
		expect(cachedDetail(H1, FRESH_MS + 1)).toBeUndefined();
	});
});
