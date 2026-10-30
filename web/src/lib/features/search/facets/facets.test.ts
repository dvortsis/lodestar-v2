import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import type { Ast } from '$lib/state/search.svelte';
import { response } from './fixtures';

const post = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { post }
}));

const { createFacets } = await import('./facets.svelte');

const word = (w: string) => ({ op: 'word', field: 'any', word: w }) as unknown as Ast;
const query = (w = 'lantern', over = {}) => ({
	ast: word(w),
	sort: 'relevance',
	hidden: false,
	lease: 'L1',
	...over
});
function deferred<T>() {
	let resolve!: (v: T) => void;
	const promise = new Promise<T>((res) => (resolve = res));
	return { promise, resolve };
}

beforeEach(() => post.mockReset());

describe('the facets store', () => {
	it('counts explicit match-all browsing on the results lease, then clears on reset', async () => {
		post.mockResolvedValue(response());
		const f = createFacets();
		const q = { ...query(), ast: { op: 'all' } as Ast, browse: true };
		await f.load(q);
		expect(post.mock.calls[0]![1].body).toMatchObject({ ast: { op: 'all' }, lease: 'L1' });
		expect(post.mock.calls[0]![1].body).not.toHaveProperty('browse');
		expect(f.data).not.toBeNull();
		await f.load({ ...q, browse: false });
		expect(f.data).toBeNull();
		expect(post).toHaveBeenCalledTimes(1);
	});

	it('posts the results’ query with their lease and the limits', async () => {
		post.mockResolvedValue(response());
		const f = createFacets();
		await f.load(query('lantern', { limits: { ext: 100 } }));
		const [path, opts] = post.mock.calls[0]!;
		expect(path).toBe('/facets');
		expect(opts.body).toMatchObject({
			ast: word('lantern'),
			q: null,
			sort: 'relevance',
			hidden: false,
			lease: 'L1',
			limits: { ext: 100 }
		});
		expect(f.data?.total).toBe(1284);
	});

	it('asks nothing for an empty query, and forgets the counts', async () => {
		post.mockResolvedValue(response());
		const f = createFacets();
		await f.load(query());
		await f.load({ ...query(), ast: null });
		expect(f.data).toBeNull();
		expect(post).toHaveBeenCalledTimes(1);
	});

	it('keeps the last counts while a newer request is out, and lets the latest win', async () => {
		const f = createFacets();
		post.mockResolvedValueOnce(response({ total: 1 }));
		await f.load(query('a'));
		const older = deferred();
		const newer = deferred();
		post.mockReturnValueOnce(older.promise).mockReturnValueOnce(newer.promise);
		const p1 = f.load(query('b'));
		expect(f.loading).toBe(true);
		expect(f.data?.total).toBe(1);
		const p2 = f.load(query('c'));
		newer.resolve(response({ total: 3 }));
		await p2;
		older.resolve(response({ total: 2 }));
		await p1;
		expect(f.data?.total).toBe(3);
		expect(f.loading).toBe(false);
	});

	it('never sends the same request twice; Retry does', async () => {
		const f = createFacets();
		post.mockRejectedValueOnce(new ApiError(503, 'unavailable', 'down'));
		await f.load(query());
		await f.load(query());
		expect(post).toHaveBeenCalledTimes(1);
		expect(f.error?.code).toBe('unavailable');
		post.mockResolvedValueOnce(response());
		await f.retry();
		expect(post).toHaveBeenCalledTimes(2);
		expect(f.error).toBeNull();
		await f.load(query());
		expect(post).toHaveBeenCalledTimes(2);
		await f.load(query('lantern', { lease: 'L2' })); // a new snapshot: counted again
		expect(post).toHaveBeenCalledTimes(3);
	});
});

describe('a stale lease', () => {
	it.each(['cursor_expired', 'bad_cursor'])(
		'on %s counts the index serving now instead',
		async (code) => {
			post.mockReset();
			post
				.mockRejectedValueOnce(new ApiError(410, code, 'gone'))
				.mockResolvedValueOnce(response({ total: 7 }));
			const f = createFacets();
			await f.load(query());
			expect(post).toHaveBeenCalledTimes(2);
			expect(post.mock.calls[1]![1].body.lease).toBeNull();
			expect(f.error).toBeNull();
			expect(f.data?.total).toBe(7);
			expect(f.loading).toBe(false);
		}
	);

	it('is not asked again: a reload or Retry sends none until a new lease comes', async () => {
		post.mockReset();
		post
			.mockRejectedValueOnce(new ApiError(410, 'cursor_expired', 'gone'))
			.mockResolvedValue(response({ total: 7 }));
		const f = createFacets();
		await f.load(query());
		await f.load(query('lantern', { limits: { ext: 100 } })); // the same lease, new limits
		await f.retry();
		expect(post.mock.calls.map((c) => c[1].body.lease)).toEqual(['L1', null, null, null]);
		await f.load(query('lantern', { lease: 'L2' }));
		expect(post.mock.calls.at(-1)![1].body.lease).toBe('L2');
	});
});
