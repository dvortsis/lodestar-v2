// The toolbar against stand-ins: W5's state, the results store and kit's page. The count is
// visual only; sort goes to W5, density to the URL, and Save opens the alerts' dialog.
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
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
const search = vi.hoisted(() => ({ q: 'lantern', sort: 'relevance', setSort: vi.fn() }));
vi.mock('$lib/state/search.svelte', async (original) => ({
	...(await original<typeof import('$lib/state/search.svelte')>()),
	search
}));
const results = vi.hoisted(() => ({
	status: 'ready',
	hits: [] as unknown[],
	total: 0,
	totalApprox: false
}));
vi.mock('../results/results.svelte', () => ({ results }));

const { default: ResultsToolbar } = await import('./ResultsToolbar.svelte');
const { visit } = await import('../query/testing.svelte');
nav.visit = visit;

beforeEach(() => {
	visit('/search?q=lantern');
	goto.mockReset();
	replaceState.mockClear();
	search.setSort.mockReset();
	search.sort = 'relevance';
	Object.assign(results, { status: 'ready', hits: [hit(1)], total: 1284, totalApprox: false });
});

const count = () => screen.getByTestId('results-count').textContent?.trim();

describe('ResultsToolbar', () => {
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

	it('writes density to the URL, replacing the entry', async () => {
		render(ResultsToolbar);
		expect(screen.getByRole('radio', { name: 'Comfortable' }).getAttribute('aria-checked')).toBe(
			'true'
		);
		await fireEvent.click(screen.getByRole('radio', { name: 'Compact' }));
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
		await fireEvent.click(screen.getByRole('radio', { name: 'Compact' }));
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
