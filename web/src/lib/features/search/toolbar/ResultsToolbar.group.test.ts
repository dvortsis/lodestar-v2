// The toolbar's "Group versions" (GS-5): the switch reads the URL, else the account's choice;
// a change is pushed to the URL and remembered; the count names groups when the rows are grouped.
import { fireEvent, render, screen } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { hit } from '../results/fixtures';

vi.setConfig({ testTimeout: 20_000 });

vi.mock('$app/state', async () => ({
	page: (await import('../query/testing.svelte')).testPage
}));
const goto = vi.hoisted(() => vi.fn());
vi.mock('$app/navigation', () => ({ goto, replaceState: vi.fn(), pushState: vi.fn() }));
const search = vi.hoisted(() => ({ q: 'lantern', sort: 'relevance', setSort: vi.fn() }));
vi.mock('$lib/state/search.svelte', async (original) => ({
	...(await original<typeof import('$lib/state/search.svelte')>()),
	search
}));
const results = vi.hoisted(() => ({
	status: 'ready',
	hits: [] as unknown[],
	total: 0,
	totalApprox: false,
	grouped: {} as Record<string, unknown>
}));
vi.mock('../results/results.svelte', () => ({ results }));
const grouping = vi.hoisted(() => ({
	pref: true,
	applied: true,
	load: vi.fn(),
	remember: vi.fn(),
	pin: vi.fn()
}));
vi.mock('../collapse/grouping.svelte', async (original) => ({
	...(await original<typeof import('../collapse/grouping.svelte')>()),
	grouping
}));

const { default: ResultsToolbar } = await import('./ResultsToolbar.svelte');
const { visit } = await import('../query/testing.svelte');

// Two switches render: the wide toolbar's and the narrow one's in its More popover (CSS shows one).
const toggles = () =>
	screen.getAllByRole('switch', { name: 'Group versions', hidden: true }) as HTMLInputElement[];
const toggle = () => toggles()[0]!;
const count = () => screen.getByTestId('results-count').textContent?.trim();

beforeEach(() => {
	visit('/search?q=lantern');
	goto.mockReset();
	grouping.remember.mockReset();
	grouping.load.mockReset();
	grouping.pref = true;
	grouping.applied = true;
	Object.assign(results, {
		status: 'ready',
		hits: [hit(1)],
		total: 30,
		totalApprox: false,
		grouped: { collapsed: true, groups: 12, groups_approx: false }
	});
});

describe('Group versions', () => {
	it('is on by default and counts groups and torrents', () => {
		render(ResultsToolbar);
		expect(toggle().checked).toBe(true);
		expect(count()).toBe('12 groups · 30 torrents');
		expect(grouping.load).toHaveBeenCalled(); // each switch asks; the store reads once
	});

	it('follows the account’s choice, and loses to the link', () => {
		grouping.applied = false;
		const a = render(ResultsToolbar);
		expect(toggles().map((t) => t.checked)).toEqual([false, false]);
		a.unmount();
		visit('/search?q=lantern&group=1');
		render(ResultsToolbar);
		expect(toggle().checked).toBe(true);
	});

	it('pushes a change to the URL and remembers it for the account', async () => {
		render(ResultsToolbar);
		await fireEvent.click(toggle());
		expect(grouping.remember).toHaveBeenCalledWith(false);
		// The entry being left writes its own value first, so Back restores it whatever the account
		// says by then; then the change is pushed.
		await vi.waitFor(() => expect(goto).toHaveBeenCalledTimes(2));
		expect(goto.mock.calls).toEqual([
			['/search?q=lantern&group=1', { keepFocus: true, noScroll: true, replaceState: true }],
			['/search?q=lantern&group=0', { keepFocus: true, noScroll: true }]
		]);
	});

	it('pushes only the change when the link already says', async () => {
		visit('/search?q=lantern&group=0');
		render(ResultsToolbar);
		await fireEvent.click(toggle());
		await vi.waitFor(() => expect(goto).toHaveBeenCalledOnce());
		expect(goto).toHaveBeenCalledWith('/search?q=lantern&group=1', {
			keepFocus: true,
			noScroll: true
		});
	});

	it('counts torrents only when the rows came ungrouped, and says why', () => {
		results.grouped = { collapsed: false, collapse_unavailable: 'group_key_not_ready' };
		render(ResultsToolbar);
		expect(count()).toBe('30 results');
		expect(toggle().checked).toBe(true);
		const note = screen.getByTestId('group-versions-unavailable');
		expect(note.textContent).toContain('still sorting torrents into versions');
		for (const t of toggles()) expect(t.getAttribute('aria-describedby')).toBe(note.id);
	});

	it('says nothing about grouping when it is off', () => {
		visit('/search?q=lantern&group=0');
		results.grouped = { collapse_unavailable: 'group_key_not_ready' };
		render(ResultsToolbar);
		expect(screen.queryByTestId('group-versions-unavailable')).toBeNull();
		expect(toggle().getAttribute('aria-describedby')).toBeNull();
	});

	it('shows ≈ for an estimated number of groups', () => {
		Object.assign(results, {
			total: 2_400_000,
			totalApprox: true,
			grouped: { collapsed: true, groups: 900_000, groups_approx: true }
		});
		render(ResultsToolbar);
		expect(count()?.replace(/\s/g, ' ')).toBe('≈ 900K groups · ≈ 2.4M torrents');
	});
});
