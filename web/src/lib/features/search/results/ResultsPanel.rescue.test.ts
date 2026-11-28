// X-zero-rescue: when the rescue's chunk fails to load, the panel still shows its "try other words"
// hint (it waits for the rescue only while the rescue can still answer).
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
vi.mock('./Rescue.svelte', () => {
	throw new Error('the rescue chunk did not load');
});
vi.mock('$lib/features/detail/commands', () => ({ registerDetailCommands: async () => {} }));

const { default: ResultsPanel } = await import('./ResultsPanel.svelte');

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

describe('ResultsPanel when the rescue chunk fails', () => {
	it('still says to try other words', async () => {
		api.post.mockResolvedValue(page([], { total: 0 }));
		const { unmount } = render(ResultsPanel, { focusBar: () => {} });
		await screen.findByTestId('results-none', {}, WAIT);
		await waitFor(
			() => expect(screen.getByText('Try fewer or different words.')).toBeTruthy(),
			WAIT
		);
		unmount();
	});
});
