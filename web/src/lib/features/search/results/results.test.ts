import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import type { Ast } from '$lib/state/search.svelte';
import { page } from './fixtures';

// Waits are conditions with a 10 s deadline (CLAUDE.md); the test's own limit must exceed it.
vi.setConfig({ testTimeout: 20_000 });

const post = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { post }
}));

const { createResults, PAGE } = await import('./results.svelte');

const term = (value: string) => ({ op: 'term', value }) as unknown as Ast;
const query = (value = 'lantern', over = {}) => ({
	ast: term(value),
	sort: 'relevance',
	hidden: false,
	exact: false,
	...over
});

/** A request the test answers by hand. */
function deferred<T>() {
	let resolve!: (v: T) => void;
	let reject!: (e: unknown) => void;
	const promise = new Promise<T>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promise, resolve, reject };
}

/** The body of the n-th request. */
const sent = (n: number) => post.mock.calls[n]![1].body;

beforeEach(() => post.mockReset());

describe('the results store', () => {
	it('is empty with no query or an empty one, and sends no request', async () => {
		const r = createResults();
		await r.load({ ...query(), ast: null });
		await r.load({ ...query(), ast: { op: 'all' } as unknown as Ast });
		expect(r.status).toBe('empty');
		expect(post).not.toHaveBeenCalled();
	});

	it('posts the query and shows the answer', async () => {
		post.mockResolvedValue(page([1, 2], { total: 1200, total_approx: true }));
		const r = createResults();
		const q = query('lantern', { sort: 'size', hidden: true });
		await r.load(q);
		const [path, opts] = post.mock.calls[0]!;
		expect(path).toBe('/search');
		expect(opts.body).toMatchObject({
			ast: q.ast,
			q: null,
			sort: 'size',
			limit: PAGE,
			cursor: null,
			hidden: true,
			exact: false
		});
		expect(r.status).toBe('ready');
		expect(r.hits.map((h) => h.id)).toEqual([1, 2]);
		expect(r.total).toBe(1200);
		expect(r.totalApprox).toBe(true);
		expect(r.showing(q)).toBe(true);
		expect(r.showing(query('other'))).toBe(false);
	});

	it('is loading with no rows to keep, refining with some', async () => {
		const r = createResults();
		const first = deferred();
		post.mockReturnValueOnce(first.promise);
		const p1 = r.load(query('a'));
		expect(r.status).toBe('loading');
		expect(r.refining).toBe(false);
		first.resolve(page([1]));
		await p1;

		const second = deferred();
		post.mockReturnValueOnce(second.promise);
		const p2 = r.load(query('b'));
		expect(r.refining).toBe(true);
		expect(r.status).toBe('ready');
		expect(r.hits.map((h) => h.id)).toEqual([1]);
		second.resolve(page([2]));
		await p2;
		expect(r.refining).toBe(false);
		expect(r.hits.map((h) => h.id)).toEqual([2]);
	});

	it('lets the latest query win when answers arrive out of order', async () => {
		const r = createResults();
		const older = deferred();
		const newer = deferred();
		post.mockReturnValueOnce(older.promise).mockReturnValueOnce(newer.promise);
		const p1 = r.load(query('a'));
		const p2 = r.load(query('b'));
		newer.resolve(page([2]));
		await p2;
		older.resolve(page([1]));
		await p1;
		expect(r.hits.map((h) => h.id)).toEqual([2]);
		expect(r.showing(query('b'))).toBe(true);
		expect(r.refining).toBe(false);
	});

	it('keeps the last rows on an error, and is an error with none', async () => {
		const r = createResults();
		post.mockRejectedValueOnce(new ApiError(503, 'unavailable', 'down'));
		await r.load(query('a'));
		expect(r.status).toBe('error');
		expect(r.error?.code).toBe('unavailable');

		post.mockResolvedValueOnce(page([1]));
		await r.retry();
		expect(r.status).toBe('ready');
		expect(r.error).toBeNull();

		post.mockRejectedValueOnce(new ApiError(500, 'internal', 'boom'));
		await r.load(query('b'));
		expect(r.status).toBe('ready');
		expect(r.error?.code).toBe('internal');
		expect(r.hits.map((h) => h.id)).toEqual([1]);
		expect(r.showing(query('b'))).toBe(false);
	});

	it('pages with the cursor and keeps the rows on a failed page', async () => {
		const r = createResults();
		post.mockResolvedValueOnce(page([1, 2], { next_cursor: 'c1' }));
		await r.load(query());
		expect(r.hasMore).toBe(true);

		post.mockResolvedValueOnce(page([3], { next_cursor: 'c2' }));
		await r.loadMore();
		expect(sent(1).cursor).toBe('c1');
		expect(r.hits.map((h) => h.id)).toEqual([1, 2, 3]);

		post.mockRejectedValueOnce(new ApiError(503, 'unavailable', 'down'));
		await r.loadMore();
		expect(r.moreError?.code).toBe('unavailable');
		expect(r.hasMore).toBe(false);
		expect(r.hits).toHaveLength(3);

		post.mockResolvedValueOnce(page([4]));
		await r.retry();
		expect(sent(3).cursor).toBe('c2');
		expect(r.hits.map((h) => h.id)).toEqual([1, 2, 3, 4]);
		expect(r.hasMore).toBe(false);
	});

	it('starts again from the top when the cursor expired', async () => {
		const r = createResults();
		post.mockResolvedValueOnce(page([1], { next_cursor: 'c1' }));
		await r.load(query());
		post.mockRejectedValueOnce(new ApiError(410, 'cursor_expired', 'gone'));
		post.mockResolvedValueOnce(page([7, 8]));
		await r.loadMore();
		expect(sent(2).cursor).toBeNull();
		expect(r.hits.map((h) => h.id)).toEqual([7, 8]);
		expect(r.moreError).toBeNull();
		expect(r.loadingMore).toBe(false);
		expect(r.expired).toBe(true);
		// The next query clears the notice.
		post.mockResolvedValueOnce(page([9]));
		await r.load(query('other'));
		expect(r.expired).toBe(false);
	});

	it('keeps the rows while an expired cursor reloads the query', async () => {
		const r = createResults();
		post.mockResolvedValueOnce(page([1], { next_cursor: 'c1' }));
		await r.load(query());
		const again = deferred();
		post.mockRejectedValueOnce(new ApiError(410, 'cursor_expired', 'gone'));
		post.mockReturnValueOnce(again.promise);
		const pm = r.loadMore();
		await vi.waitFor(() => expect(post).toHaveBeenCalledTimes(3), { timeout: 10_000 });
		expect(r.hits.map((h) => h.id)).toEqual([1]);
		expect(r.refining).toBe(true);
		again.resolve(page([5]));
		await pm;
		expect(r.hits.map((h) => h.id)).toEqual([5]);
	});

	it('does not ask again for the query in flight or the one shown', async () => {
		const r = createResults();
		const first = deferred();
		post.mockReturnValueOnce(first.promise);
		const p1 = r.load(query('a'));
		void r.load(query('a')); // W5's URL write right after its `ast`: an equal, new object
		first.resolve(page([1], { next_cursor: 'c1' }));
		await p1;
		post.mockResolvedValueOnce(page([2]));
		await r.loadMore();
		await r.load(query('a')); // a density change re-runs the effect: the pages stay
		expect(post).toHaveBeenCalledTimes(2);
		expect(r.hits.map((h) => h.id)).toEqual([1, 2]);
	});

	it('waits for Retry after a failure instead of asking again', async () => {
		const r = createResults();
		post.mockRejectedValueOnce(new ApiError(503, 'unavailable', 'down'));
		await r.load(query('a'));
		await r.load(query('a'));
		expect(post).toHaveBeenCalledTimes(1);
		post.mockResolvedValueOnce(page([1]));
		await r.retry();
		expect(post).toHaveBeenCalledTimes(2);
		expect(r.status).toBe('ready');
	});

	it('names the query the rows answer', async () => {
		const r = createResults();
		post.mockResolvedValueOnce(page([1]));
		await r.load(query('a'));
		const shown = r.shown;
		const later = deferred();
		post.mockReturnValueOnce(later.promise);
		const p2 = r.load(query('b'));
		expect(r.shown).toBe(shown); // refining: still the old rows
		later.resolve(page([2]));
		await p2;
		expect(r.shown).not.toBe(shown);
	});

	it('drops a page that answers an older query', async () => {
		const r = createResults();
		post.mockResolvedValueOnce(page([1], { next_cursor: 'c1' }));
		await r.load(query('a'));
		const more = deferred();
		post.mockReturnValueOnce(more.promise);
		const pm = r.loadMore();
		post.mockResolvedValueOnce(page([9]));
		await r.load(query('b'));
		more.resolve(page([2]));
		await pm;
		expect(r.hits.map((h) => h.id)).toEqual([9]);
	});

	it('passes the adult_hidden notice through', async () => {
		const r = createResults();
		post.mockResolvedValueOnce(page([], { notices: ['adult_hidden'] }));
		await r.load(query());
		expect(r.status).toBe('ready');
		expect(r.notices).toEqual(['adult_hidden']);
	});
});
