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
// The row actions load lazily with the first rows: let them land before the file ends.
afterAll(async () => {
	await import('./RowActions.svelte');
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
