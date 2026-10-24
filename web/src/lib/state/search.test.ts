import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createSearchState, readUrl, writeUrl, type Ast, type Nav } from './search.svelte';

const json = (body: unknown, init: ResponseInit = {}) =>
	new Response(JSON.stringify(body), { headers: { 'Content-Type': 'application/json' }, ...init });

const all: Ast = { op: 'all' };
const word = (w: string): Ast => ({ op: 'word', field: 'any', word: w });

/** A tiny history: go() pushes or replaces, back() restores the previous entry. */
function fakeNav(start = 'http://x/search') {
	const stack = [new URL(start)];
	const calls: { url: string; push: boolean }[] = [];
	const nav: Nav = {
		url: () => stack.at(-1)!,
		go: async (url, push) => {
			calls.push({ url, push });
			const u = new URL(url, 'http://x');
			if (push) stack.push(u);
			else stack[stack.length - 1] = u;
		}
	};
	return {
		nav,
		calls,
		back: () => void stack.pop(),
		visit: (u: string) => void stack.push(new URL(u))
	};
}

let fetchMock: ReturnType<typeof vi.fn>;
beforeEach(() => {
	vi.useFakeTimers();
	fetchMock = vi.fn(async (u: string) => {
		const q = new URL(u, 'http://x').searchParams.get('q') ?? '';
		if (q.includes('(')) {
			return json(
				{ error: { code: 'query_syntax', message: 'bad', field: 'q', start: 1, end: 2 } },
				{ status: 400 }
			);
		}
		return json({ ast: q ? word(q) : all, canonical: q.trim().toLowerCase() });
	});
	vi.stubGlobal('fetch', fetchMock);
});
afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

describe('URL contract', () => {
	it('reads defaults and values', () => {
		expect(readUrl(new URLSearchParams(''))).toEqual({
			q: '',
			sort: 'relevance',
			hidden: false,
			exact: false
		});
		expect(readUrl(new URLSearchParams('q=a+b&sort=-size&hidden=1&exact=true'))).toEqual({
			q: 'a b',
			sort: '-size',
			hidden: true,
			exact: true
		});
	});

	it('falls back to defaults for invalid values', () => {
		const s = readUrl(new URLSearchParams('sort=nonsense&hidden=maybe&exact=2'));
		expect(s).toEqual({ q: '', sort: 'relevance', hidden: false, exact: false });
		expect(readUrl(new URLSearchParams('sort=%2Bfiles')).sort).toBe('+files');
	});

	it('round-trips, omitting defaults and keeping unknown parameters', () => {
		const base = new URL('http://x/search?utm=1&q=old&sort=bogus&view=compact#top');
		const out = writeUrl(base, { q: 'new one', sort: '-size', hidden: true, exact: false });
		const u = new URL(out, 'http://x');
		expect(u.searchParams.get('q')).toBe('new one');
		expect(u.searchParams.get('utm')).toBe('1');
		expect(u.searchParams.get('view')).toBe('compact');
		expect(u.hash).toBe('#top');
		expect(readUrl(u.searchParams)).toEqual({
			q: 'new one',
			sort: '-size',
			hidden: true,
			exact: false
		});
		const plain = new URL(
			writeUrl(base, { q: '', sort: 'relevance', hidden: false, exact: false }),
			'http://x'
		);
		expect(plain.search).toBe('?utm=1&view=compact');
	});
});

describe('search state', () => {
	it('typing debounces, parses once, and replaces the history entry with the canonical text', async () => {
		const f = fakeNav();
		const s = createSearchState(f.nav);
		s.setText('U');
		s.setText('Ub');
		s.setText('Ubuntu ');
		await vi.advanceTimersByTimeAsync(199);
		expect(fetchMock).not.toHaveBeenCalled();
		await vi.advanceTimersByTimeAsync(1);
		expect(fetchMock).toHaveBeenCalledTimes(1);
		expect(s.text).toBe('Ubuntu '); // the draft isn't rewritten while typing
		expect(s.ast).toEqual(word('Ubuntu '));
		expect(f.calls).toEqual([{ url: '/search?q=ubuntu', push: false }]);
		expect(s.q).toBe('ubuntu');
	});

	it('exposes a parse error with its position and keeps the raw text in the URL', async () => {
		const f = fakeNav();
		const s = createSearchState(f.nav);
		s.setText('a (');
		await vi.advanceTimersByTimeAsync(200);
		expect(s.parseError).toEqual({ code: 'query_syntax', message: 'bad', start: 1, end: 2 });
		expect(s.ast).toBeNull();
		expect(f.calls[0]).toEqual({ url: '/search?q=a+%28', push: false });
		s.setText('a');
		await vi.advanceTimersByTimeAsync(200);
		expect(s.parseError).toBeNull();
	});

	it('submit parses now, normalises the draft and pushes', async () => {
		const f = fakeNav();
		const s = createSearchState(f.nav);
		s.setText('  Linux ');
		await s.submit();
		expect(s.text).toBe('linux');
		expect(f.calls).toEqual([{ url: '/search?q=linux', push: true }]);
		await vi.advanceTimersByTimeAsync(500);
		expect(f.calls).toHaveLength(1); // the pending debounce was cancelled
	});

	it('a stale parse cannot overwrite a newer one', async () => {
		const f = fakeNav();
		let release!: () => void;
		fetchMock.mockImplementationOnce(
			() => new Promise((r) => (release = () => r(json({ ast: word('slow'), canonical: 'slow' }))))
		);
		const s = createSearchState(f.nav);
		s.setText('slow');
		await vi.advanceTimersByTimeAsync(200);
		s.setText('fast');
		await vi.advanceTimersByTimeAsync(200);
		release();
		await vi.advanceTimersByTimeAsync(0);
		expect(s.ast).toEqual(word('fast'));
		expect(s.q).toBe('fast');
		expect(f.calls.map((c) => c.url)).toEqual(['/search?q=fast']);
	});

	it('setAst posts the AST and uses the response canonical as q', async () => {
		const f = fakeNav('http://x/search?q=old&sort=-size');
		fetchMock.mockImplementationOnce(async (_u: string, init: RequestInit) => {
			expect(init.method).toBe('POST');
			expect((init.headers as Record<string, string>)['Content-Type']).toBe('application/json');
			expect(JSON.parse(init.body as string).ast).toEqual(word('new'));
			return json({ canonical: 'new', hits: [] });
		});
		const s = createSearchState(f.nav);
		await s.setAst(word('new'));
		expect(s.ast).toEqual(word('new'));
		expect(s.text).toBe('new');
		expect(f.calls).toEqual([{ url: '/search?q=new&sort=-size', push: true }]);
	});

	it('setAst can clear hidden in the same history entry', async () => {
		const f = fakeNav('http://x/search?q=old+res%3A1080p&hidden=1');
		fetchMock.mockImplementationOnce(async () => json({ canonical: 'old', hits: [] }));
		const s = createSearchState(f.nav);
		await s.setAst(word('old'), { hidden: false });
		expect(f.calls).toEqual([{ url: '/search?q=old', push: true }]);
	});

	it('typing during a structured edit wins: the edit is dropped and the draft parses', async () => {
		const f = fakeNav('http://x/search?q=lantern+res');
		let release!: () => void;
		fetchMock.mockImplementationOnce(
			() => new Promise((r) => (release = () => r(json({ canonical: 'lantern', hits: [] }))))
		);
		const s = createSearchState(f.nav);
		const edit = s.setAst(word('lantern'));
		s.setText('lantern more'); // typed before the server answered the edit
		release();
		await edit;
		expect(s.text).toBe('lantern more');
		expect(f.calls).toEqual([]); // the edit committed nothing
		await vi.advanceTimersByTimeAsync(200);
		expect(s.ast).toEqual(word('lantern more'));
		expect(f.calls).toEqual([{ url: '/search?q=lantern+more', push: false }]);
	});

	it('a parse answer for an older draft never becomes the AST', async () => {
		const f = fakeNav();
		let release!: () => void;
		fetchMock.mockImplementationOnce(
			() => new Promise((r) => (release = () => r(json({ ast: word('old'), canonical: 'old' }))))
		);
		const s = createSearchState(f.nav);
		s.setText('old');
		await vi.advanceTimersByTimeAsync(200); // the parse of "old" is in flight
		s.setText('new');
		release();
		await vi.advanceTimersByTimeAsync(0);
		expect(s.ast).toBeNull(); // not "old"
		await vi.advanceTimersByTimeAsync(200);
		expect(s.ast).toEqual(word('new'));
	});

	it('setAst clears parsing on success and on error', async () => {
		const f = fakeNav();
		const s = createSearchState(f.nav);
		await s.setAst(word('a'));
		expect(s.parsing).toBe(false);
		fetchMock.mockImplementationOnce(async () =>
			json({ error: { code: 'bad_ast', message: 'x' } }, { status: 400 })
		);
		await expect(s.setAst(word('b'))).rejects.toMatchObject({ code: 'bad_ast' });
		expect(s.parsing).toBe(false);
	});

	it('a submit superseded by a newer submit does not push its raw text', async () => {
		const f = fakeNav();
		const releases: (() => void)[] = [];
		fetchMock.mockImplementation(
			(u: string) =>
				new Promise((resolve) => {
					const q = new URL(u, 'http://x').searchParams.get('q') ?? '';
					releases.push(() => resolve(json({ ast: word(q), canonical: q.trim().toLowerCase() })));
				})
		);
		const s = createSearchState(f.nav);
		s.setText('Foo  ');
		const a = s.submit();
		const b = s.submit();
		releases.forEach((r) => r());
		await Promise.all([a, b]);
		expect(f.calls).toEqual([{ url: '/search?q=foo', push: true }]);
	});

	it('a submit superseded by setAst does nothing', async () => {
		const f = fakeNav();
		let release!: () => void;
		fetchMock.mockImplementationOnce(
			() => new Promise((r) => (release = () => r(json({ ast: word('x'), canonical: 'x' }))))
		);
		const s = createSearchState(f.nav);
		s.setText('X ');
		const a = s.submit();
		fetchMock.mockImplementationOnce(async () => json({ canonical: 'new', hits: [] }));
		await s.setAst(word('new'));
		release();
		await a;
		expect(f.calls).toEqual([{ url: '/search?q=new', push: true }]);
	});

	it('does not push a duplicate entry when the URL is unchanged', async () => {
		const f = fakeNav('http://x/search?q=a');
		const s = createSearchState(f.nav);
		await s.setSort('relevance');
		await s.setHidden(false);
		expect(f.calls).toEqual([]);
	});

	it('back and forward restore the draft from the URL without navigating', async () => {
		const f = fakeNav();
		const s = createSearchState(f.nav);
		s.setText('one');
		await s.submit();
		s.setText('two');
		await s.submit();
		expect(f.calls.map((c) => c.push)).toEqual([true, true]);
		const before = f.calls.length;
		f.back();
		s.sync();
		await vi.advanceTimersByTimeAsync(0);
		expect(s.q).toBe('one');
		expect(s.text).toBe('one');
		expect(s.ast).toEqual(word('one'));
		expect(f.calls).toHaveLength(before);
	});

	it('adopts the URL on first sync and exposes its parse error', async () => {
		const f = fakeNav('http://x/search?q=a+(&sort=-files&exact=1&utm=z');
		const s = createSearchState(f.nav);
		s.sync();
		await vi.advanceTimersByTimeAsync(0);
		expect(s.text).toBe('a (');
		expect(s.parseError?.code).toBe('query_syntax');
		expect(s.sort).toBe('-files');
		expect(s.exact).toBe(true);
		s.sync();
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});

	it('does not throw on an invalid sort and drops it on the next write', async () => {
		const f = fakeNav('http://x/search?q=a&sort=nonsense');
		const s = createSearchState(f.nav);
		expect(s.sort).toBe('relevance');
		await s.setHidden(true);
		expect(f.calls[0]!.url).toBe('/search?q=a&hidden=1');
	});

	it('option changes push and keep q and unknown parameters', async () => {
		const f = fakeNav('http://x/search?q=a&view=x');
		const s = createSearchState(f.nav);
		await s.setSort('+size');
		await s.setExact(true);
		expect(f.calls).toEqual([
			{ url: '/search?q=a&view=x&sort=%2Bsize', push: true },
			{ url: '/search?q=a&view=x&sort=%2Bsize&exact=1', push: true }
		]);
	});

	it('reset clears the query and options but keeps unknown parameters', async () => {
		const f = fakeNav('http://x/search?q=a&sort=-size&hidden=1&view=x');
		const s = createSearchState(f.nav);
		s.sync();
		await vi.advanceTimersByTimeAsync(0);
		await s.reset();
		expect(s.text).toBe('');
		expect(s.ast).toBeNull();
		expect(f.calls.at(-1)).toEqual({ url: '/search?view=x', push: true });
	});
});
