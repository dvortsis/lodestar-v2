// The panel with grouped results (GS-5): it asks for groups, shows "+N other versions" on a
// grouped row, and opens the group in place, where → and ← work from the keyboard.
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { stubLayout } from '$lib/ui/lists/testing/layout';
import { hit, page } from './fixtures';

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
// The list's commands (j/k/x), kept so a test can run them as the key would.
const commands = vi.hoisted(() => new Map<string, () => void>());
vi.mock('$lib/keys', () => ({
	keys: {
		register: (c: { id: string; run: () => void }) => (commands.set(c.id, c.run), () => {})
	}
}));
const openDetail = vi.hoisted(() => vi.fn());
vi.mock('$lib/features/detail/open', () => ({
	openDetail,
	prefetchDetail: () => {},
	cancelDetailPrefetch: () => {},
	returnFocusTarget: () => null
}));
vi.mock('$lib/features/detail/commands', () => ({ registerDetailCommands: async () => {} }));

const { default: ResultsPanel } = await import('./ResultsPanel.svelte');
const { visit } = await import('../query/testing.svelte');

beforeAll(() => stubLayout(600, 1000));
afterAll(async () => {
	await Promise.all([
		import('./RowActions.svelte'),
		import('./commands'),
		import('$lib/keys'),
		import('../collapse/rows'),
		import('../table/ResultsTable.svelte')
	]);
});

const best = hit(1, { others: 2 });
const versions = [hit(11), hit(12)];
const siblings = (hits: ReturnType<typeof hit>[], truncated = false) => ({
	other_versions: { hits: [best, ...hits], reason: null, status: 'available', truncated }
});

beforeEach(() => {
	visit('/search?q=lantern');
	api.get.mockReset();
	api.post.mockReset();
	openDetail.mockReset();
	api.get.mockImplementation(async (path: string) => {
		if (path === '/torrents/{hash}/siblings') return siblings(versions);
		throw new Error(`no ${path} in this test`);
	});
	api.post.mockResolvedValue(
		page([], { hits: [best, hit(2)], total: 5, collapsed: true, groups: 2, groups_approx: false })
	);
});

const version = (n: number) => `result-group:${best.hash}:${hit(n).hash}`;
const state = (kind: string) => `result-group:${best.hash}:${kind}`;
const rowOf = (testid: string) => screen.getByTestId(testid).closest<HTMLElement>('[data-index]')!;
const announced = () => screen.getByTestId('results-announce').textContent;
/** Siblings answered by `fn`; every other GET fails, as in beforeEach. */
function mockSiblings(fn: () => Promise<unknown>) {
	api.get.mockImplementation(async (path: string) => {
		if (path === '/torrents/{hash}/siblings') return fn();
		throw new Error(`no ${path} in this test`);
	});
}
/** Renders the panel and opens the best row's group from its "+N". */
async function openGroup() {
	render(ResultsPanel, { focusBar: () => {} });
	const others = await screen.findByTestId(`result-${best.hash}-others`, {}, WAIT);
	// The count is said first (a frame after the rows), as it is before anyone can press.
	await waitFor(() => expect(announced()).toBe('2 groups · 5 torrents'), WAIT);
	await fireEvent.click(others);
	return others;
}

describe('ResultsPanel, grouped', () => {
	it('asks for groups by default, and leaves them out when the link says so', async () => {
		const a = render(ResultsPanel, { focusBar: () => {} });
		await waitFor(() => expect(api.post).toHaveBeenCalled(), WAIT);
		expect(api.post.mock.calls[0]![1].body.collapse).toBe('group');
		a.unmount();
		api.post.mockClear();
		visit('/search?q=lantern&group=0');
		render(ResultsPanel, { focusBar: () => {} });
		await waitFor(() => expect(api.post).toHaveBeenCalled(), WAIT);
		expect(api.post.mock.calls[0]![1].body).not.toHaveProperty('collapse');
	});

	it('opens a group in place from "+N other versions", and closes it again', async () => {
		render(ResultsPanel, { focusBar: () => {} });
		const others = await screen.findByTestId(`result-${best.hash}-others`, {}, WAIT);
		expect(others.textContent?.trim()).toBe('+2 other versions');
		// Only grouped rows carry it.
		expect(screen.queryByTestId(`result-${hit(2).hash}-others`)).toBeNull();
		await fireEvent.click(others);
		await screen.findByTestId(version(11), {}, WAIT);
		expect(screen.getByTestId(version(12))).toBeTruthy();
		expect(others.getAttribute('aria-expanded')).toBe('true');
		expect(api.get).toHaveBeenCalledWith('/torrents/{hash}/siblings', {
			path: { hash: best.hash },
			signal: expect.any(AbortSignal)
		});
		// The versions sit between their best row and the next result.
		const order = [...document.querySelectorAll<HTMLElement>('[data-index]')].map((el) =>
			el.querySelector('[data-hash]')?.getAttribute('data-hash')
		);
		expect(order).toEqual([best.hash, hit(11).hash, hit(12).hash, hit(2).hash]);
		await fireEvent.click(others);
		await waitFor(() => expect(screen.queryByTestId(version(11))).toBeNull(), WAIT);
	});

	it('→ opens the focused group, ← on a version closes it and focuses the best row', async () => {
		render(ResultsPanel, { focusBar: () => {} });
		await screen.findByTestId(`result-${best.hash}-others`, {}, WAIT);
		const row = rowOf(`result-${best.hash}`);
		row.focus();
		await fireEvent.keyDown(row, { key: 'ArrowRight' });
		await screen.findByTestId(version(11), {}, WAIT);
		const v = rowOf(version(11));
		v.focus();
		await fireEvent.keyDown(v, { key: 'ArrowLeft' });
		await waitFor(() => expect(screen.queryByTestId(version(11))).toBeNull(), WAIT);
		await waitFor(() => expect(document.activeElement).toBe(rowOf(`result-${best.hash}`)), WAIT);
	});

	it('shows no disclosure when the answer is ungrouped', async () => {
		api.post.mockResolvedValue(
			page([], {
				hits: [best],
				total: 1,
				collapsed: false,
				collapse_unavailable: 'group_key_not_ready'
			})
		);
		render(ResultsPanel, { focusBar: () => {} });
		await screen.findByTestId(`result-${best.hash}`, {}, WAIT);
		expect(screen.queryByTestId(`result-${best.hash}-others`)).toBeNull();
	});

	it('is a tree grid while grouped: levels, open state, and what a group loaded is said', async () => {
		render(ResultsPanel, { focusBar: () => {} });
		const others = await screen.findByTestId(`result-${best.hash}-others`, {}, WAIT);
		await waitFor(() => expect(announced()).toBe('2 groups · 5 torrents'), WAIT);
		await fireEvent.click(others);
		const grid = screen.getByRole('treegrid');
		await screen.findByTestId(version(11), {}, WAIT);
		const best1 = rowOf(`result-${best.hash}`);
		expect(best1.getAttribute('aria-level')).toBe('1');
		expect(best1.getAttribute('aria-expanded')).toBe('true');
		expect(rowOf(version(11)).getAttribute('aria-level')).toBe('2');
		// A row with no other versions has nothing to open.
		expect(rowOf(`result-${hit(2).hash}`).hasAttribute('aria-expanded')).toBe(false);
		expect(grid.contains(others)).toBe(true);
		await waitFor(() => expect(announced()).toBe('2 other versions shown'), WAIT);
		await fireEvent.click(others);
		await waitFor(() => expect(best1.getAttribute('aria-expanded')).toBe('false'), WAIT);
	});

	it('is a plain grid when the rows came ungrouped', async () => {
		visit('/search?q=lantern&group=0');
		api.post.mockResolvedValue(page([], { hits: [hit(2)], total: 1 }));
		render(ResultsPanel, { focusBar: () => {} });
		await screen.findByTestId(`result-${hit(2).hash}`, {}, WAIT);
		expect(screen.queryByRole('treegrid')).toBeNull();
		expect(rowOf(`result-${hit(2).hash}`).hasAttribute('aria-level')).toBe(false);
	});

	it('Show all opens the best version’s details at Related', async () => {
		mockSiblings(async () => siblings([11, 12, 13, 14, 15, 16].map((n) => hit(n))));
		await openGroup();
		const more = await screen.findByTestId(`${state('more')}-more`, {}, WAIT);
		// Six versions listed though two match the query: never fewer than the rows it stands for.
		expect(more.textContent).toContain('Show all 7');
		expect(screen.queryByTestId(version(16))).toBeNull(); // five at most in place
		await fireEvent.keyDown(rowOf(state('more')), { key: 'Enter' });
		await waitFor(() => expect(openDetail).toHaveBeenCalled(), WAIT);
		expect(openDetail.mock.calls[0]![0]).toBe(best.hash);
		expect(openDetail.mock.calls[0]![1]).toMatchObject({ section: 'related' });
	});

	it('says a second group’s load again when it has the same words', async () => {
		const two = hit(2, { others: 2 });
		// Each group has two other versions (the best row isn't among them here).
		mockSiblings(async () => ({
			other_versions: { hits: versions, reason: null, status: 'available', truncated: false }
		}));
		api.post.mockResolvedValue(
			page([], { hits: [best, two], total: 6, collapsed: true, groups: 2, groups_approx: false })
		);
		render(ResultsPanel, { focusBar: () => {} });
		await screen.findByTestId(`result-${best.hash}-others`, {}, WAIT);
		await waitFor(() => expect(announced()).toBe('2 groups · 6 torrents'), WAIT);
		await fireEvent.click(screen.getByTestId(`result-${best.hash}-others`));
		await waitFor(() => expect(announced()).toBe('2 other versions shown'), WAIT);
		await fireEvent.click(screen.getByTestId(`result-${two.hash}-others`));
		// Cleared first, then the same words: a screen reader hears the second load too.
		await waitFor(() => expect(announced()).toBe(''), WAIT);
		await waitFor(() => expect(announced()).toBe('2 other versions shown'), WAIT);
	});

	it('a version opens its own details', async () => {
		await openGroup();
		await screen.findByTestId(version(12), {}, WAIT);
		await fireEvent.keyDown(rowOf(version(12)), { key: 'Enter' });
		await waitFor(() => expect(openDetail).toHaveBeenCalled(), WAIT);
		expect(openDetail.mock.calls[0]![0]).toBe(hit(12).hash);
	});

	it('a failed group says so, and Enter on its line tries again', async () => {
		let calls = 0;
		mockSiblings(async () => {
			if (++calls === 1) throw new Error('offline');
			return siblings(versions);
		});
		await openGroup();
		const failed = await screen.findByTestId(state('failed'), {}, WAIT);
		expect(failed.textContent).toContain('Retry');
		await waitFor(() => expect(announced()).toBe("Other versions couldn't load."), WAIT);
		await fireEvent.keyDown(rowOf(state('failed')), { key: 'Enter' });
		await screen.findByTestId(version(11), {}, WAIT);
		expect(calls).toBe(2);
		expect(openDetail).not.toHaveBeenCalled();
	});

	it('x selects a torrent row, never a version or a group’s line', async () => {
		const { selection } = await import('../bulk/selection.svelte');
		await openGroup();
		await screen.findByTestId(version(11), {}, WAIT);
		await waitFor(() => expect(commands.get('results.select')).toBeDefined(), WAIT);
		// The arrows make a row active, as j/k do.
		await fireEvent.keyDown(rowOf(`result-${best.hash}`), { key: 'ArrowDown' });
		commands.get('results.select')!();
		expect(selection.count).toBe(0);
		await fireEvent.keyDown(rowOf(version(11)), { key: 'ArrowUp' });
		commands.get('results.select')!();
		expect(selection.count).toBe(1);
		commands.get('results.select')!(); // leave nothing selected for the next test
		expect(selection.count).toBe(0);
	});

	it('with the details open, j/k walk the versions and step over a group’s line', async () => {
		mockSiblings(async () => siblings(versions, true)); // cut short: a Show all line follows v12
		visit('/search?q=lantern', { detail: { hash: best.hash } });
		await openGroup();
		await screen.findByTestId(state('more'), {}, WAIT);
		await screen.findByTestId(version(11), {}, WAIT);
		await waitFor(() => expect(commands.get('results.next')).toBeDefined(), WAIT);
		commands.get('results.next')!();
		await waitFor(
			() => expect(openDetail).toHaveBeenLastCalledWith(hit(11).hash, expect.anything()),
			WAIT
		);
		commands.get('results.next')!();
		commands.get('results.next')!();
		await waitFor(
			() => expect(openDetail).toHaveBeenLastCalledWith(hit(2).hash, expect.anything()),
			WAIT
		);
		openDetail.mockClear();
		commands.get('results.previous')!(); // v12
		commands.get('results.previous')!(); // v11
		commands.get('results.previous')!(); // best
		await waitFor(
			() => expect(openDetail).toHaveBeenLastCalledWith(best.hash, expect.anything()),
			WAIT
		);
	});

	it('steps over a failed group’s line with the details open', async () => {
		mockSiblings(async () => {
			throw new Error('offline');
		});
		visit('/search?q=lantern', { detail: { hash: best.hash } });
		await openGroup();
		await screen.findByTestId(state('failed'), {}, WAIT);
		await waitFor(() => expect(commands.get('results.next')).toBeDefined(), WAIT);
		commands.get('results.next')!();
		await waitFor(() => expect(openDetail).toHaveBeenCalledOnce(), WAIT);
		expect(openDetail.mock.calls[0]![0]).toBe(hit(2).hash);
	});

	it('stays on the last torrent at the end of the list rather than on a group’s line', async () => {
		mockSiblings(async () => siblings(versions, true));
		api.post.mockResolvedValue(
			page([], { hits: [hit(2), best], total: 5, collapsed: true, groups: 2, groups_approx: false })
		);
		visit('/search?q=lantern', { detail: { hash: best.hash } });
		await openGroup();
		await screen.findByTestId(state('more'), {}, WAIT);
		await waitFor(() => expect(commands.get('results.next')).toBeDefined(), WAIT);
		for (let i = 0; i < 6; i++) commands.get('results.next')!();
		await waitFor(
			() => expect(openDetail).toHaveBeenLastCalledWith(hit(12).hash, expect.anything()),
			WAIT
		);
		expect(openDetail).not.toHaveBeenCalledWith(
			best.hash,
			expect.objectContaining({ section: 'related' })
		);
	});

	it('keeps the same torrent active across a change of view', async () => {
		api.post.mockResolvedValue(
			page([], { hits: [best, hit(2), hit(3)], total: 5, collapsed: true, groups: 2 })
		);
		await openGroup();
		await screen.findByTestId(version(12), {}, WAIT);
		// hit(2) is the List's fourth row (best, v11, v12, hit 2), the Table's second.
		await fireEvent.keyDown(rowOf(`result-${hit(2).hash}`), { key: 'ArrowDown' });
		await fireEvent.keyDown(rowOf(`result-${hit(3).hash}`), { key: 'ArrowUp' });
		visit('/search?q=lantern&view=table');
		const table = await screen.findByRole('table', {}, WAIT);
		await waitFor(() => expect(table.querySelectorAll('tbody tr').length).toBe(3), WAIT);
		await waitFor(() => expect(commands.get('results.select')).toBeDefined(), WAIT);
		const { selection } = await import('../bulk/selection.svelte');
		try {
			commands.get('results.select')!();
			expect(selection.has(hit(2).hash)).toBe(true);
		} finally {
			selection.toggle(hit(2).hash, false); // leave nothing selected for the next test
		}
	});

	it('the Table shows one row per group, with an open group left to the List', async () => {
		await openGroup();
		await screen.findByTestId(version(11), {}, WAIT);
		visit('/search?q=lantern&view=table');
		await waitFor(() => expect(screen.queryByTestId(version(11))).toBeNull(), WAIT);
		const table = await screen.findByRole('table', {}, WAIT);
		await waitFor(() => expect(table.querySelectorAll('tbody tr').length).toBe(2), WAIT);
		expect(table.textContent).not.toContain(hit(11).name);
	});
});
