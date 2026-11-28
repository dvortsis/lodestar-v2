// The toolbar against stand-ins: W5's state, the results store and kit's page. The count is
// visual only; sort goes to W5, density to the URL, and Save opens the alerts' dialog.
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { hit } from '../results/fixtures';

vi.setConfig({ testTimeout: 20_000 });
const WAIT = { timeout: 10_000 };

vi.mock('$app/state', async () => ({
	page: (await import('../query/testing.svelte')).testPage
}));
const goto = vi.hoisted(() => vi.fn());
// kit's replaceState moves page.url: the stand-in does too.
const nav = vi.hoisted(() => ({ visit: (() => {}) as (to: string) => void }));
const replaceState = vi.hoisted(() => vi.fn((to: string) => nav.visit(to)));
vi.mock('$app/navigation', () => ({ goto, replaceState, pushState: vi.fn() }));
const search = vi.hoisted(() => ({
	q: 'lantern',
	sort: 'relevance',
	hidden: false,
	setSort: vi.fn(),
	setHidden: vi.fn()
}));
vi.mock('$lib/state/search.svelte', async (original) => ({
	...(await original<typeof import('$lib/state/search.svelte')>()),
	search
}));
vi.mock('../results/results.svelte', async () => ({
	results: (await import('./testing.svelte')).fakeResults
}));
const post = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { post }
}));
const { fakeResults: results } = await import('./testing.svelte');

const { default: ResultsToolbar } = await import('./ResultsToolbar.svelte');
const { visit } = await import('../query/testing.svelte');
nav.visit = visit;

beforeEach(() => {
	visit('/search?q=lantern');
	goto.mockReset();
	replaceState.mockClear();
	search.setSort.mockReset();
	search.sort = 'relevance';
	search.hidden = false;
	search.setHidden.mockReset();
	post.mockReset();
	Object.assign(results, {
		status: 'ready',
		hits: [hit(1)],
		total: 1284,
		totalApprox: false,
		query: null,
		grouped: {}
	});
});

const count = () => screen.getByTestId('results-count').textContent?.trim();

describe('ResultsToolbar', () => {
	const lantern = { ast: { op: 'term', value: 'lantern' }, hidden: false };
	const asked = () => post.mock.calls.filter(([path]) => path === '/search/hidden');

	it('asks for the hidden count once the rows show, and Show adds them (P7)', async () => {
		post.mockResolvedValue({ hidden_total: 1200 });
		results.query = lantern;
		render(ResultsToolbar);
		await waitFor(() => expect(screen.getByTestId('results-hidden-show')).toBeTruthy(), WAIT);
		expect(asked()).toHaveLength(1);
		expect(asked()[0]![1].body).toMatchObject({ ast: lantern.ast, hidden: false });
		// Both forms are in the line; CSS shows one by the toolbar's width (the short one is aria-hidden).
		const short = screen.getByTestId('results-count').querySelector('.tb-short')!;
		expect(short.textContent).toBe('1,200 filtered');
		expect(short.getAttribute('aria-hidden')).toBe('true');
		expect(count()?.replace(short.textContent!, '').replace(/\s+/g, ' ')).toBe(
			'1,284 results · 1,200 hidden by filters · Show'
		);
		const show = screen.getByRole('button', { name: 'Show' });
		const described = document.getElementById(show.getAttribute('aria-describedby')!);
		expect(described?.textContent).toBe('1,200 hidden by filters');
		const region = document.body.appendChild(
			Object.assign(document.createElement('div'), { id: 'results' })
		);
		await fireEvent.click(show);
		expect(search.setHidden).toHaveBeenCalledWith(true);
		expect(document.activeElement).toBe(region);
		region.remove();
	});

	it('drops a late answer for rows no longer shown, and aborts its request', async () => {
		let answer!: (v: unknown) => void;
		const signals: AbortSignal[] = [];
		post.mockImplementationOnce(
			(_: string, o: { signal: AbortSignal }) => (
				signals.push(o.signal),
				new Promise((r) => (answer = r))
			)
		);
		post.mockResolvedValueOnce({ hidden_total: 3 });
		results.query = lantern;
		render(ResultsToolbar);
		await waitFor(() => expect(asked()).toHaveLength(1), WAIT);
		// A new query's rows: the old request is aborted, its late answer ignored.
		results.query = { ast: { op: 'term', value: 'other' }, hidden: false };
		await waitFor(() => expect(asked()).toHaveLength(2), WAIT);
		expect(signals[0]!.aborted).toBe(true);
		answer({ hidden_total: 999 });
		await waitFor(
			() => expect(screen.getByTestId('results-hidden').textContent).toBe('3 hidden by filters'),
			WAIT
		);
		await tick();
		expect(screen.getByTestId('results-hidden').textContent).toBe('3 hidden by filters');
	});

	it('hides the count while a new query loads or fails, so Show never applies to it', async () => {
		post.mockResolvedValue({ hidden_total: 3 });
		results.query = lantern;
		render(ResultsToolbar);
		await waitFor(() => expect(screen.getByTestId('results-hidden-show')).toBeTruthy(), WAIT);
		// The next query has no rows to keep: the store says loading, the rows' query stays.
		results.status = 'loading';
		await tick();
		expect(screen.queryByTestId('results-hidden-show')).toBeNull();
		results.status = 'error';
		await tick();
		expect(screen.queryByTestId('results-hidden-show')).toBeNull();
		results.status = 'ready';
	});

	it('offers nothing for none, an unknown count, a failure, or hidden already shown', async () => {
		for (const [shown, answer] of [
			[lantern, () => Promise.resolve({ hidden_total: 0 })],
			[lantern, () => Promise.resolve({ hidden_total: null })],
			[lantern, () => Promise.reject(new Error('down'))],
			[{ ...lantern, hidden: true }, () => Promise.resolve({ hidden_total: 5 })]
		] as const) {
			post.mockReset();
			post.mockImplementation(answer);
			results.query = shown;
			const { unmount } = render(ResultsToolbar);
			if (!shown.hidden) {
				await waitFor(() => expect(asked()).toHaveLength(1), WAIT);
				// Settled, so nothing can still arrive after the check.
				await (post.mock.results[0]!.value as Promise<unknown>).catch(() => {});
			}
			await tick();
			expect(screen.queryByTestId('results-hidden')).toBeNull();
			expect(screen.queryByTestId('results-hidden-show')).toBeNull();
			unmount();
		}
		// Already shown: nothing is asked.
		expect(asked()).toHaveLength(0);
	});

	it('counts the results: exact, one, none, and ≈ compact only when the server says so', () => {
		const { unmount } = render(ResultsToolbar);
		expect(count()).toBe('1,284 results');
		unmount();
		results.total = 1;
		const one = render(ResultsToolbar);
		expect(count()).toBe('1 result');
		one.unmount();
		Object.assign(results, { total: 2_400_000, totalApprox: true });
		render(ResultsToolbar);
		expect(count()).toBe('≈ 2.4M results');
	});

	it('says nothing while the first answer is out, and hides with no query', () => {
		results.status = 'loading';
		const first = render(ResultsToolbar);
		expect(count()).toBe('');
		expect(screen.getByTestId('results-count').getAttribute('aria-live')).toBeNull();
		first.unmount();
		results.status = 'empty';
		render(ResultsToolbar);
		expect(screen.queryByTestId('results-toolbar')).toBeNull();
	});

	it('says 0 results for no matches, whatever the total', () => {
		Object.assign(results, { hits: [], total: 7 });
		render(ResultsToolbar);
		expect(count()).toBe('0 results');
	});

	it('shows W5’s sort, a bare field as descending, and hands a change to W5', async () => {
		search.sort = 'discovered';
		render(ResultsToolbar);
		const select = screen.getByLabelText('Sort') as HTMLSelectElement;
		expect(select.value).toBe('-discovered');
		await fireEvent.change(select, { target: { value: '+size' } });
		expect(search.setSort).toHaveBeenCalledWith('+size');
	});

	it('writes the view to the URL, replacing the entry', async () => {
		render(ResultsToolbar);
		expect((screen.getByLabelText('View') as HTMLSelectElement).value).toBe('list');
		await fireEvent.change(screen.getByLabelText('View'), { target: { value: 'compact' } });
		expect(goto).toHaveBeenCalledWith('/search?q=lantern&density=compact', {
			replaceState: true,
			keepFocus: true,
			noScroll: true
		});
	});

	it('from the drawer’s shallow URL, puts it back on /search before writing', async () => {
		visit('/t/' + 'a'.repeat(40) + '?q=lantern');
		render(ResultsToolbar);
		await fireEvent.change(screen.getByLabelText('Sort'), { target: { value: '-size' } });
		expect(replaceState).toHaveBeenLastCalledWith('/search?q=lantern', {});
		expect(search.setSort).toHaveBeenCalledWith('-size');
		visit('/t/' + 'a'.repeat(40) + '?q=lantern');
		await fireEvent.change(screen.getByLabelText('View'), { target: { value: 'compact' } });
		await waitFor(
			() =>
				expect(goto).toHaveBeenCalledWith('/search?q=lantern&density=compact', expect.anything()),
			WAIT
		);
	});

	it('opens Save search with the query in its field', async () => {
		render(ResultsToolbar);
		await fireEvent.click(screen.getByRole('button', { name: 'Save search' }));
		await waitFor(() => expect(screen.getByRole('dialog')).toBeTruthy(), WAIT);
		expect((screen.getByLabelText('Query') as HTMLInputElement).value).toBe('lantern');
	});
});

describe('the More popover (below 640 px)', () => {
	it('is a native disclosure holding Select and Save', () => {
		render(ResultsToolbar);
		const more = screen.getByRole('button', { name: 'More', hidden: true });
		const pop = document.getElementById(more.getAttribute('popovertarget')!)!;
		expect(pop.getAttribute('popover')).toBe('auto');
		expect([...pop.querySelectorAll('button')].map((b) => b.textContent?.trim())).toEqual([
			'Select',
			'Save search'
		]);
	});
});

describe('the account density preference', () => {
	afterEach(() => {
		delete document.documentElement.dataset.density;
	});

	it('is the default view of a link that names none, and writes nothing to the URL', async () => {
		document.documentElement.dataset.density = 'compact';
		render(ResultsToolbar);
		await tick();
		expect((screen.getByLabelText('View') as HTMLSelectElement).value).toBe('compact');
		expect(goto).not.toHaveBeenCalled();
	});

	it('loses to a view the link names', () => {
		document.documentElement.dataset.density = 'cards';
		visit('/search?q=lantern&density=comfortable');
		render(ResultsToolbar);
		expect((screen.getByLabelText('View') as HTMLSelectElement).value).toBe('list');
	});

	it('does not move the view when the shell applies it late', async () => {
		render(ResultsToolbar);
		document.documentElement.dataset.density = 'cards';
		await tick();
		expect((screen.getByLabelText('View') as HTMLSelectElement).value).toBe('list');
	});

	it('writes the pick, and keeps a List the user chose under another preference', async () => {
		document.documentElement.dataset.density = 'compact';
		render(ResultsToolbar);
		await fireEvent.change(screen.getByLabelText('View'), { target: { value: 'list' } });
		expect(goto).toHaveBeenCalledWith('/search?q=lantern&density=comfortable', {
			replaceState: true,
			keepFocus: true,
			noScroll: true
		});
	});
});
