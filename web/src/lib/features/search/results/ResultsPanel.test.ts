import { configure, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { components } from '$lib/api/schema';

vi.mock('$app/state', async () => ({
	page: (await import('../query/testing.svelte')).testPage
}));
vi.mock('$app/navigation', () => ({ goto: vi.fn(), replaceState: vi.fn(), pushState: vi.fn() }));

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get, post: vi.fn() }
}));

// The shell's status, replaced by a reactive stand-in whose rebuild snapshot the test sets.
vi.mock('$lib/shell/status.svelte', async () => ({
	shellStatus: (await import('./testing.svelte')).testStatus
}));

// The key registry and S-detail's commands load after mount; the panel's tests don't use them.
vi.mock('$lib/keys', () => ({ keys: { register: () => () => {} } }));
vi.mock('$lib/features/detail/commands', () => ({ registerDetailCommands: async () => {} }));

const { default: ResultsPanel } = await import('./ResultsPanel.svelte');
const { visit } = await import('../query/testing.svelte');
const { setRebuild } = await import('./testing.svelte');

type Figures = components['schemas']['StatsResponse'];
const figures = (searchable: number | null): Figures => ({
	data_bytes: null,
	disk_free_bytes: null,
	fetched_per_hour: null,
	harvested_per_hour: null,
	index_lag_s: null,
	paused: false,
	queue_depth: 0,
	searchable_torrents: searchable,
	stored_torrents: 0,
	updated_at: 0,
	uptime_s: 0
});
const saved = (id: number) => ({ id, name: `Saved ${id}`, q: `q${id}`, sort: null });

function serve(s: Figures, list: unknown[] = []) {
	get.mockImplementation(async (path: string) => {
		if (path === '/stats') return s;
		if (path === '/saved-searches') return list;
		throw new Error(`unexpected GET ${path}`);
	});
}

// Waits are conditions with a 10 s deadline (CLAUDE.md), not the 1 s default.
configure({ asyncUtilTimeout: 10_000 });

beforeEach(() => {
	get.mockReset();
	setRebuild(null);
	visit('/search');
});

describe('ResultsPanel, the bare page', () => {
	it('says how many torrents there are, with examples and up to five saved searches', async () => {
		serve(figures(2_400_000), [1, 2, 3, 4, 5, 6].map(saved));
		render(ResultsPanel, { focusBar: () => {} });
		await waitFor(() =>
			expect(screen.getByTestId('results-empty-title').textContent?.trim()).toBe(
				'Search 2,400,000 torrents'
			)
		);
		await waitFor(() =>
			expect(screen.getByTestId('results-saved').querySelectorAll('a')).toHaveLength(5)
		);
		expect(
			screen.getByTestId('results-empty').querySelectorAll('.rp-examples')[0]!.children
		).toHaveLength(3);
	});

	it('does without the figures when they fail to load', async () => {
		get.mockRejectedValue(new Error('down'));
		render(ResultsPanel, { focusBar: () => {} });
		await waitFor(() => expect(get).toHaveBeenCalledTimes(2));
		expect(screen.getByTestId('results-empty-title').textContent?.trim()).toBe('Search torrents');
	});

	it('shows a rebuild in progress, and hides it once done', async () => {
		serve(figures(10));
		render(ResultsPanel, { focusBar: () => {} });
		const event = { done: 1200, total: 4000, error: null, from: 'a', to: 'b', unavailable: [] };
		setRebuild({ ...event, phase: 'building' });
		await waitFor(() =>
			expect(screen.getByTestId('results-rebuilding').textContent).toContain('1,200 of 4,000')
		);
		setRebuild({ ...event, phase: 'done' });
		await waitFor(() => expect(screen.queryByTestId('results-rebuilding')).toBeNull());
	});

	it('calls the bar on “Start typing”', async () => {
		serve(figures(null));
		const focusBar = vi.fn();
		render(ResultsPanel, { focusBar });
		screen.getByRole('button', { name: 'Start typing' }).click();
		expect(focusBar).toHaveBeenCalled();
	});
});
