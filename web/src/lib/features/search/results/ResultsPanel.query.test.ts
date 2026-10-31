// The panel with a query: its polite announcement, and what a remount asks for. W5's search state
// is a fixed stand-in (the query already parsed), the API a mock.
import { render, screen, waitFor } from '@testing-library/svelte';
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { stubLayout } from '$lib/ui/lists/testing/layout';
import { page } from './fixtures';

// Waits are conditions with a 10 s deadline (CLAUDE.md); the test's own limit must exceed it.
vi.setConfig({ testTimeout: 20_000 });
const WAIT = { timeout: 10_000 };

vi.mock('$app/state', async () => ({
	page: (await import('../query/testing.svelte')).testPage
}));
vi.mock('$app/navigation', () => ({ goto: vi.fn(), replaceState: vi.fn(), pushState: vi.fn() }));
vi.mock('$lib/state/search.svelte', () => ({
	search: {
		q: 'lantern',
		queryReady: true,
		ast: { op: 'word', field: 'any', word: 'lantern' },
		sort: 'relevance',
		hidden: false,
		exact: false
	}
}));
const api = vi.hoisted(() => ({ get: vi.fn(), post: vi.fn() }));
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api
}));
vi.mock('$lib/shell/status.svelte', async () => ({
	shellStatus: (await import('./testing.svelte')).testStatus
}));
vi.mock('$lib/keys', () => ({ keys: { register: () => () => {} } }));
vi.mock('$lib/features/detail/commands', () => ({ registerDetailCommands: async () => {} }));

const { default: ResultsPanel } = await import('./ResultsPanel.svelte');
const { results } = await import('./results.svelte');

beforeAll(() => stubLayout(600, 1000));
// The row actions, key commands and detail module load lazily from mount, rows and hover: let
// them land before the file ends, or the environment can be torn down mid-import.
afterAll(async () => {
	await Promise.all([
		import('./RowActions.svelte'),
		import('./commands'),
		import('$lib/keys'),
		import('$lib/features/detail/open')
	]);
});
beforeEach(() => {
	api.get.mockReset();
	api.post.mockReset();
	api.get.mockRejectedValue(new Error('no figures in this test'));
});

/** Every text the polite region has held, in order (an announcement is "", then its text). */
function said() {
	const region = screen.getByTestId('results-announce');
	const seen: string[] = [];
	new MutationObserver(() => seen.push(region.textContent ?? '')).observe(region, {
		childList: true,
		characterData: true,
		subtree: true
	});
	return seen;
}

describe('ResultsPanel with a query', () => {
	it('a words-only zero-result query says to try other words, with no filter lead', async () => {
		api.post.mockResolvedValue(page([], { total: 0 }));
		const { unmount } = render(ResultsPanel, { focusBar: () => {} });
		await screen.findByTestId('results-none', {}, WAIT);
		expect(screen.getByText('Try fewer or different words.')).toBeTruthy();
		expect(screen.queryByText('Remove a filter to widen the search:')).toBeNull();
		unmount();
	});

	it('announces the count once per answered query, not again for a new page', async () => {
		api.post
			.mockResolvedValueOnce(page([1, 2], { total: 60, next_cursor: 'c1' }))
			.mockResolvedValueOnce(page([3, 4]));
		const { unmount } = render(ResultsPanel, { focusBar: () => {} });
		const seen = said();
		await waitFor(() => expect(seen).toContain('60 torrents'), WAIT);
		const before = seen.length;
		await results.loadMore();
		await waitFor(() => expect(results.hits).toHaveLength(4), WAIT);
		await new Promise(requestAnimationFrame); // an announcement would have landed by now
		expect(seen.slice(before)).toEqual([]);
		unmount();
	});

	it('Load more asks for the next page once, keeps focus, and hands it to the end line', async () => {
		let answer!: (r: unknown) => void;
		api.post
			.mockResolvedValueOnce(page([1, 2], { total: 3, next_cursor: 'c1' }))
			.mockImplementationOnce(() => new Promise((r) => (answer = r)));
		render(ResultsPanel, { focusBar: () => {} });
		const more = await screen.findByRole('button', { name: 'Load more torrents' }, WAIT);
		more.focus();
		more.click(); // the sentinel may have asked already: either way, one request
		more.click();
		await waitFor(() => expect(api.post).toHaveBeenCalledTimes(2), WAIT);
		expect(more.getAttribute('aria-busy')).toBe('true');
		expect(document.activeElement).toBe(more);
		answer(page([3]));
		const end = await screen.findByTestId('results-end', {}, WAIT);
		await waitFor(() => expect(document.activeElement).toBe(end), WAIT);
		expect(screen.queryByRole('button', { name: 'Load more torrents' })).toBeNull();
		expect(api.post).toHaveBeenCalledTimes(2);
	});

	it('a failed page moves focus to Retry, and Retry back to Load more', async () => {
		const { ApiError } = await import('$lib/api/client');
		let answer!: (r: unknown) => void;
		api.post
			.mockResolvedValueOnce(page([1, 2], { total: 9, next_cursor: 'c1' }))
			.mockRejectedValueOnce(new ApiError(503, 'unavailable', 'down'))
			.mockImplementationOnce(() => new Promise((r) => (answer = r)));
		render(ResultsPanel, { focusBar: () => {} });
		const more = await screen.findByRole('button', { name: 'Load more torrents' }, WAIT);
		more.focus();
		more.click();
		const retry = await screen.findByRole('button', { name: 'Retry' }, WAIT);
		await waitFor(() => expect(document.activeElement).toBe(retry), WAIT);
		retry.click();
		await waitFor(
			() =>
				expect(document.activeElement).toBe(
					screen.getByRole('button', { name: 'Load more torrents' })
				),
			WAIT
		);
		answer(page([3], { next_cursor: 'c2' }));
		await waitFor(() => expect(results.hits).toHaveLength(3), WAIT);
	});

	it('drops the old rows on unmount: a failed request after a remount shows none of them', async () => {
		api.post.mockResolvedValue(page([1, 2]));
		const first = render(ResultsPanel, { focusBar: () => {} });
		await waitFor(() => expect(screen.getAllByRole('row')).toHaveLength(2), WAIT);
		first.unmount();
		expect(results.hits).toEqual([]);
		// e.g. "Show adult content" was turned off meanwhile, and the new request fails.
		const { ApiError } = await import('$lib/api/client');
		api.post.mockRejectedValue(new ApiError(503, 'unavailable', 'down'));
		render(ResultsPanel, { focusBar: () => {} });
		await waitFor(() => expect(screen.getByTestId('results-error')).toBeTruthy(), WAIT);
		expect(screen.queryAllByRole('row')).toHaveLength(0);
		expect(results.status).toBe('error');
	});

	it('asks again after a remount: answers from before it may be stale', async () => {
		api.post.mockResolvedValue(page([1]));
		const first = render(ResultsPanel, { focusBar: () => {} });
		await waitFor(() => expect(api.post).toHaveBeenCalledTimes(1), WAIT);
		await waitFor(() => expect(results.status).toBe('ready'), WAIT);
		first.unmount();
		api.post.mockResolvedValue(page([9]));
		render(ResultsPanel, { focusBar: () => {} });
		await waitFor(() => expect(api.post).toHaveBeenCalledTimes(2), WAIT);
		await waitFor(() => expect(results.hits.map((h) => h.id)).toEqual([9]), WAIT);
	});
});
