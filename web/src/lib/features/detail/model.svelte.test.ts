import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import { HASH, searchHit, torrentDetail } from './fixtures';
import { cacheDetail, cacheHit, cachedDetail, cachedHit, clearDetailCaches } from './cache';

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get }
}));
vi.mock('./visibility', () => ({ watchDetailVisibility: vi.fn() }));
const { DetailModel } = await import('./model.svelte');
const flush = async () => {
	for (let i = 0; i < 20; i++) await Promise.resolve();
};
let model: InstanceType<typeof DetailModel>;
beforeEach(() => {
	clearDetailCaches(false);
	get.mockReset();
	model = new DetailModel();
});
afterEach(() => model.destroy());

describe('detail visibility', () => {
	it('revalidates on close/reopen and withholds both caches until success', async () => {
		cacheHit(HASH, searchHit());
		cacheDetail(HASH, torrentDetail());
		let finish!: (value: unknown) => void;
		get.mockImplementation(
			() =>
				new Promise((resolve) => {
					finish = resolve;
				})
		);
		model.load(HASH);
		expect(model.title).toBe('');
		expect(model.hit).toBeNull();
		expect(model.detail).toBeNull();
		finish(torrentDetail());
		await flush();
		expect(model.title).toBe('Show');
		model.destroy();
		model.load(HASH);
		expect(get).toHaveBeenCalledTimes(2);
		expect(model.title).toBe('');
		finish(torrentDetail());
		await flush();
		expect(model.title).toBe('Show');
	});

	it.each([401, 403, 404, 503, 0])(
		'withholds and purges caches on failed revalidation (%s)',
		async (status) => {
			cacheHit(HASH, searchHit());
			cacheDetail(HASH, torrentDetail());
			get.mockRejectedValue(new ApiError(status, 'unavailable', 'Unavailable'));
			model.load(HASH);
			await flush();
			expect(model.status).toBe('error');
			expect(model.title).toBe('');
			expect(cachedHit(HASH)).toBeUndefined();
			expect(cachedDetail(HASH)).toBeUndefined();
			expect(get).toHaveBeenCalledTimes(1);
		}
	);

	it('clears a mounted detail immediately and revalidates after visibility invalidation', async () => {
		cacheHit(HASH, searchHit());
		get.mockResolvedValue(torrentDetail());
		model.load(HASH);
		await flush();
		expect(model.title).toBe('Show');
		get.mockRejectedValue(new ApiError(404, 'adult_hidden', 'Hidden'));
		clearDetailCaches();
		expect(model.title).toBe('');
		expect(model.detail).toBeNull();
		await flush();
		expect(model.status).toBe('error');
	});

	it('drops a delayed hit response after invalidation even if abort loses the race', async () => {
		let finish!: (value: unknown) => void;
		let signal!: AbortSignal;
		get.mockImplementation((path: string, options: { signal: AbortSignal }) => {
			if (path === '/torrents/{hash}') return Promise.resolve(torrentDetail());
			signal = options.signal;
			return new Promise((resolve) => {
				finish = resolve;
			});
		});
		model.load(HASH);
		await flush();
		clearDetailCaches(false);
		expect(signal.aborted).toBe(true);
		finish({ hits: [searchHit()] });
		await flush();
		expect(model.title).toBe('');
		expect(cachedHit(HASH)).toBeUndefined();
		expect(cachedDetail(HASH)).toBeUndefined();
	});

	it.each(['invalidate', 'close'] as const)('drops a delayed detail after %s', async (action) => {
		let finish!: (value: unknown) => void;
		let signal!: AbortSignal;
		get.mockImplementation((_path: string, options: { signal: AbortSignal }) => {
			signal = options.signal;
			return new Promise((resolve) => {
				finish = resolve;
			});
		});
		model.load(HASH);
		if (action === 'close') model.destroy();
		else clearDetailCaches(false);
		expect(signal.aborted).toBe(true);
		finish(torrentDetail());
		await flush();
		expect(model.title).toBe('');
		expect(cachedDetail(HASH)).toBeUndefined();
	});
});
