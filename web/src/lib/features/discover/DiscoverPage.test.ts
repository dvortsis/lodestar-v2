import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { tick } from 'svelte';
import { ApiError } from '$lib/api/client';
import { keys } from '$lib/keys';
import { stubLayout } from '$lib/ui/lists/testing/layout';

const get = vi.hoisted(() => vi.fn());
const goto = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { get }
}));
vi.mock('$app/navigation', () => ({ goto }));

import DiscoverPage from './DiscoverPage.svelte';
import { group, hit, hash, newest, response, unavailable, type DiscoverResponse } from './testing';

beforeAll(() => stubLayout(720));

type Query = { type?: string; cursor?: string };
let handler: (query: Query) => DiscoverResponse | Promise<DiscoverResponse>;
let settings: unknown;

beforeEach(() => {
	handler = () => response();
	settings = { fields: { show_adult: { value: false } } };
	get.mockReset();
	goto.mockReset();
	get.mockImplementation(async (path: string, opts?: { query?: Query }) => {
		if (path === '/settings/{section}') {
			if (settings instanceof Error) throw settings;
			return settings;
		}
		return handler(opts?.query ?? {});
	});
});
afterEach(() => {
	cleanup();
	vi.useRealTimers();
});

const discoverCalls = () =>
	get.mock.calls.filter(([p]) => p === '/discover').map(([, o]) => o?.query ?? {});
const mount = (search = '') => render(DiscoverPage, { search: new URLSearchParams(search) });
const apiError = (status: number, code: string) => new ApiError(status, code, code);

describe('tabs and URL state', () => {
	it('asks for every type when type is absent, and marks All current', async () => {
		mount();
		await screen.findByTestId('discover-newest-total');
		expect(discoverCalls()).toEqual([{}]);
		const all = screen.getByTestId('discover-tab-all');
		expect(all.getAttribute('aria-current')).toBe('page');
		expect(screen.getByRole('navigation', { name: 'Type' })).toBeTruthy();
		expect(all.getAttribute('href')).toBe('/discover');
		expect(screen.getByTestId('discover-tab-movie').getAttribute('href')).toBe(
			'/discover?type=movie'
		);
	});

	it('asks for a known tab and marks it current, keeping density on the links', async () => {
		mount('type=music&density=compact');
		await screen.findByTestId('discover-newest-total');
		expect(discoverCalls()).toEqual([{ type: 'music' }]);
		expect(screen.getByTestId('discover-tab-music').getAttribute('aria-current')).toBe('page');
		expect(screen.getByTestId('discover-tab-all').getAttribute('aria-current')).toBeNull();
		expect(screen.getByTestId('discover-tab-tv').getAttribute('href')).toBe(
			'/discover?type=tv&density=compact'
		);
	});

	it('restarts from the first page when the tab changes', async () => {
		const view = mount('type=movie');
		await screen.findByTestId('discover-newest-total');
		handler = () => response({ newest: newest({ hits: [hit(9)], type: 'tv' }) });
		await view.rerender({ search: new URLSearchParams('type=tv') });
		await screen.findByTestId(`discover-row-${hash(9)}`);
		expect(discoverCalls()).toEqual([{ type: 'movie' }, { type: 'tv' }]);
	});

	it('shows "That type doesn\'t exist" with a link to All on unknown_type', async () => {
		handler = () => {
			throw apiError(400, 'unknown_type');
		};
		mount('type=bogus');
		await screen.findByRole('heading', { name: "That type doesn't exist" });
		expect(screen.getByRole('link', { name: 'Show all types' }).getAttribute('href')).toBe(
			'/discover'
		);
	});

	it('links "See all in Search" to the query Newest runs', async () => {
		mount('type=movie');
		const link = await screen.findByTestId('discover-see-all');
		expect(link.getAttribute('href')).toBe('/search?q=type%3Amovie&sort=-discovered');
	});
});

describe('the Adult tab', () => {
	it('is shown only when show_adult is true', async () => {
		settings = { fields: { show_adult: { value: true } } };
		mount();
		expect(await screen.findByTestId('discover-tab-adult')).toBeTruthy();
	});

	it('is absent when show_adult is false', async () => {
		mount();
		await screen.findByTestId('discover-newest-total');
		await waitFor(() => expect(get).toHaveBeenCalledWith('/settings/{section}', expect.anything()));
		expect(screen.queryByTestId('discover-tab-adult')).toBeNull();
	});

	it('is absent when settings cannot be read', async () => {
		settings = apiError(500, 'internal');
		mount();
		await screen.findByTestId('discover-newest-total');
		expect(screen.queryByTestId('discover-tab-adult')).toBeNull();
	});

	it('explains an empty adult tab from the server notice, never as zero', async () => {
		handler = () =>
			response({
				newest: newest({ hits: [], total: 0, notices: ['adult_hidden'], type: 'adult' })
			});
		mount('type=adult');
		await screen.findByText(/Adult content is hidden/);
		expect(screen.getByRole('link', { name: 'Open settings' }).getAttribute('href')).toBe(
			'/settings/content'
		);
		expect(screen.queryByTestId('discover-newest-total')).toBeNull();
		expect(screen.queryByText('Nothing discovered yet')).toBeNull();
	});
});

describe('Newest', () => {
	it('shows the exact total, and ≈ with the compact form when approximate', async () => {
		handler = () => response({ newest: newest({ total: 1284 }) });
		const view = mount();
		expect((await screen.findByTestId('discover-newest-total')).textContent).toBe('1,284');
		view.unmount();
		handler = () => response({ newest: newest({ total: 2_400_000, total_approx: true }) });
		mount();
		await waitFor(() =>
			expect(screen.getByTestId('discover-newest-total').textContent).toBe('≈ 2.4M')
		);
	});

	it('sends type with the cursor, appends the next page, and stops at a null cursor', async () => {
		const first = Array.from({ length: 50 }, (_, i) => hit(i + 1));
		handler = (q) =>
			q.cursor
				? response({ newest: newest({ hits: [hit(51), hit(52)], total: 52, next_cursor: null }) })
				: response({ newest: newest({ hits: first, total: 52, next_cursor: 'c1' }) });
		mount('type=movie');
		await screen.findByTestId('discover-newest-total');
		const list = screen.getByRole('listbox');
		list.scrollTop = 100000;
		await fireEvent.scroll(list);
		await waitFor(() => expect(discoverCalls()).toContainEqual({ type: 'movie', cursor: 'c1' }));
		await waitFor(() => expect(list.getAttribute('aria-busy')).toBeNull());
		expect(discoverCalls().filter((q) => q.cursor)).toHaveLength(1);
	});

	it('starts over, with a notice, when the cursor expired', async () => {
		let calls = 0;
		let expired = false;
		handler = (q) => {
			if (q.cursor && !expired) {
				expired = true;
				throw apiError(410, 'cursor_expired');
			}
			if (q.cursor) return response({ newest: newest({ hits: [hit(99)], total: 80 }) });
			calls++;
			return response({
				newest: newest({
					hits: Array.from({ length: 50 }, (_, i) => hit(i + 1)),
					total: 80,
					next_cursor: 'c1'
				})
			});
		};
		mount();
		await screen.findByTestId('discover-newest-total');
		const list = screen.getByRole('listbox');
		list.scrollTop = 100000;
		await fireEvent.scroll(list);
		await screen.findByText(/started again from the top/);
		await waitFor(() => expect(calls).toBe(2));
		// The restart lands once; it is not aborted by a re-armed load-more (no request loop).
		await new Promise((r) => setTimeout(r, 150));
		expect(calls).toBe(2);
		expect(discoverCalls().filter((q) => !q.cursor)).toHaveLength(2);
		expect(screen.getByTestId(`discover-row-${hash(1)}`)).toBeTruthy();
	});

	it('keeps the rows, clears hasMore and offers Retry when paging fails', async () => {
		let fail = true;
		handler = (q) => {
			if (q.cursor && fail) throw apiError(0, 'network');
			return q.cursor
				? response({ newest: newest({ hits: [hit(51)], total: 51, next_cursor: null }) })
				: response({
						newest: newest({
							hits: Array.from({ length: 50 }, (_, i) => hit(i + 1)),
							total: 51,
							next_cursor: 'c1'
						})
					});
		};
		mount();
		await screen.findByTestId('discover-newest-total');
		const list = screen.getByRole('listbox');
		list.scrollTop = 100000;
		await fireEvent.scroll(list);
		await screen.findByText(/Couldn't load more torrents/);
		expect(screen.getByTestId(`discover-row-${hash(1)}`)).toBeTruthy();
		const before = discoverCalls().length;
		await tick();
		expect(discoverCalls().length).toBe(before); // no retry loop
		fail = false;
		await fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
		await waitFor(() => expect(discoverCalls().length).toBe(before + 1));
		await waitFor(() => expect(screen.queryByText(/Couldn't load more torrents/)).toBeNull());
	});

	it('keeps the last rows and offers Retry when a refresh fails', async () => {
		const view = mount();
		await screen.findByTestId(`discover-row-${hash(1)}`);
		handler = () => {
			throw apiError(502, 'http_502');
		};
		await view.rerender({ search: new URLSearchParams('type=movie') });
		await screen.findByText(/Couldn't load Discover/);
		expect(screen.getByTestId(`discover-row-${hash(1)}`)).toBeTruthy(); // stale rows stay
		handler = () => response({ newest: newest({ hits: [hit(7)] }) });
		await fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
		await screen.findByTestId(`discover-row-${hash(7)}`);
	});

	it("never sends one tab's cursor with another tab, and ignores a page that was in flight", async () => {
		let release: () => void = () => {};
		handler = (q) =>
			q.cursor
				? new Promise((resolve) => {
						release = () => resolve(response({ newest: newest({ hits: [hit(90)], total: 80 }) }));
					})
				: response({
						newest: newest({
							hits: q.type ? [hit(70)] : Array.from({ length: 50 }, (_, i) => hit(i + 1)),
							total: q.type ? 1 : 80,
							next_cursor: q.type ? null : 'c1'
						})
					});
		const view = mount();
		await screen.findByTestId('discover-newest-total');
		const list = screen.getByRole('listbox');
		list.scrollTop = 100000;
		await fireEvent.scroll(list);
		await waitFor(() => expect(discoverCalls()).toContainEqual({ cursor: 'c1' }));
		await view.rerender({ search: new URLSearchParams('type=tv') });
		await screen.findByTestId(`discover-row-${hash(70)}`);
		release();
		await new Promise((r) => setTimeout(r, 50));
		expect(screen.queryByTestId(`discover-row-${hash(90)}`)).toBeNull();
		expect(discoverCalls().every((q) => !q.cursor || !q.type)).toBe(true);
		expect(discoverCalls().filter((q) => q.type)).toEqual([{ type: 'tv' }]);
	});

	it('says search is off, and where to turn it on, on 503 search_disabled', async () => {
		handler = () => {
			throw apiError(503, 'search_disabled');
		};
		mount();
		await screen.findByRole('heading', { name: 'Search is turned off' });
		expect(screen.getByRole('link', { name: 'Open search settings' }).getAttribute('href')).toBe(
			'/settings/search'
		);
	});

	it('shows 8 skeleton rows before the first answer', async () => {
		handler = () => new Promise(() => {});
		mount();
		expect(screen.getAllByTestId('skeleton').length).toBeGreaterThanOrEqual(8);
		expect(screen.getByRole('heading', { name: 'Newest' })).toBeTruthy();
		expect(document.querySelector('[aria-busy="true"]')).toBeTruthy();
	});

	it('shows the designed empty state on All and on a tab', async () => {
		handler = () => response({ newest: newest({ hits: [], total: 0 }) });
		const view = mount();
		await screen.findByRole('heading', { name: 'Nothing discovered yet' });
		expect(screen.getByRole('link', { name: 'Open Insights' }).getAttribute('href')).toBe(
			'/insights'
		);
		view.unmount();
		mount('type=comic');
		await screen.findByRole('heading', { name: 'No comic torrents yet' });
		expect(screen.getByRole('link', { name: 'Show all types' })).toBeTruthy();
	});

	it('opens the torrent from a row, and only for a real hash', async () => {
		handler = () => response({ newest: newest({ hits: [hit(1), hit(2, { hash: 'nope' })] }) });
		mount('density=compact');
		const row = (await screen.findByTestId(`discover-row-${hash(1)}`)).closest('[role="option"]')!;
		await fireEvent.click(row);
		expect(goto).toHaveBeenCalledWith(`/t/${hash(1)}?density=compact`);
		goto.mockReset();
		await fireEvent.click(screen.getByTestId('discover-row-nope').closest('[role="option"]')!);
		expect(goto).not.toHaveBeenCalled();
	});
});

describe('rows', () => {
	it('shows never-scraped as — with its accessible name', async () => {
		handler = () =>
			response({
				newest: newest({
					hits: [
						hit(1, {
							seeders: null,
							leechers: null,
							trackers_checked: null,
							trackers_reporting: null
						})
					]
				})
			});
		mount();
		const never = await screen.findByTestId(`discover-row-${hash(1)}-never-scraped`);
		expect(never.textContent).toBe('—');
		expect(never.getAttribute('aria-label')).toBe('Never scraped');
	});

	it('shows 0 as 0, not as never scraped', async () => {
		handler = () => response({ newest: newest({ hits: [hit(1, { seeders: 0, leechers: 0 })] }) });
		mount();
		expect((await screen.findByTestId(`discover-row-${hash(1)}-seeders`)).textContent).toBe('0');
	});

	it('uses the raw name when title is null, and shows the raw name and method when they differ', async () => {
		handler = () =>
			response({
				newest: newest({
					hits: [hit(1, { title: null }), hit(2, { resolved_via: 'files' })]
				})
			});
		mount();
		expect((await screen.findByTestId(`discover-row-${hash(1)}-title`)).textContent).toBe(
			'Raw.Name.1.2160p'
		);
		const second = screen.getByTestId(`discover-row-${hash(2)}`);
		expect(within(second).getByText('Raw.Name.2.2160p')).toBeTruthy();
		expect(within(second).getByText('resolved via files')).toBeTruthy();
	});

	it('labels an empty composition "Not classified yet" and shows the dominant share otherwise', async () => {
		handler = () => response({ newest: newest({ hits: [hit(1, { composition: [] }), hit(2)] }) });
		mount();
		await screen.findByTestId(`discover-row-${hash(1)}`);
		expect(
			within(screen.getByTestId(`discover-row-${hash(1)}`)).getByText('Not classified yet')
		).toBeTruthy();
		expect(
			within(screen.getByTestId(`discover-row-${hash(2)}`)).getByText('82% video')
		).toBeTruthy();
	});

	it('marks a backfill discovery', async () => {
		handler = () => response({ newest: newest({ hits: [hit(1, { flags: ['backfill'] })] }) });
		mount();
		const row = await screen.findByTestId(`discover-row-${hash(1)}`);
		expect(within(row).getByText('First seen during backfill')).toBeTruthy();
	});

	it('uses the single-line Compact row without the composition bar', async () => {
		mount('density=compact');
		const row = await screen.findByTestId(`discover-row-${hash(1)}`);
		expect(row.querySelector('[data-density="compact"]')).toBeTruthy();
		expect(screen.queryByTestId(`discover-row-${hash(1)}-composition`)).toBeNull();
	});
});

describe('Rising and Recently resolved', () => {
	it('words every unavailable section as a compact notice', async () => {
		mount();
		const rising = await screen.findByTestId('discover-rising-unavailable');
		expect(rising.textContent).toMatch(/enough seeder counts/);
		expect(screen.getByTestId('discover-resolved-unavailable').textContent).toMatch(
			/doesn't record when a torrent gets identified/
		);
		expect(rising.textContent).toMatch(/Not available/);
	});

	it('offers the one next step when search is off', async () => {
		handler = () => response({ rising: unavailable('search_disabled') });
		mount();
		const notice = await screen.findByTestId('discover-rising-unavailable');
		expect(within(notice).getByRole('link', { name: 'Turn on search' }).getAttribute('href')).toBe(
			'/settings/search'
		);
	});

	it('lists an available section as rows that link to the torrent', async () => {
		handler = () =>
			response({
				rising: group({ hits: [hit(21), hit(22)], truncated: true }),
				recently_resolved: group({ hits: [hit(31)] })
			});
		mount();
		const link = await screen.findByTestId(`discover-rising-${hash(21)}`);
		expect(link.getAttribute('href')).toBe(`/t/${hash(21)}`);
		expect(screen.getByText('Ranked by seeders gained per day over the last week.')).toBeTruthy();
		expect(screen.getByText('More matched than are listed here.')).toBeTruthy();
		expect(screen.getByTestId(`discover-resolved-${hash(31)}`)).toBeTruthy();
	});
});

describe('the first-load reveal', () => {
	const mql = (reduce: boolean) =>
		vi.stubGlobal('matchMedia', (q: string) => ({
			matches: reduce && q.includes('reduce'),
			media: q,
			addEventListener() {},
			removeEventListener() {}
		}));
	afterEach(() => vi.unstubAllGlobals());

	it('reveals at most 8 rows, 80 ms apart, on the first data only', async () => {
		mql(false);
		handler = () =>
			response({ newest: newest({ hits: Array.from({ length: 12 }, (_, i) => hit(i + 1)) }) });
		const view = mount();
		await screen.findByTestId(`discover-row-${hash(1)}`);
		const revealed = () => document.querySelectorAll('.dsc-reveal');
		await waitFor(() => expect(revealed().length).toBeGreaterThan(0));
		expect(revealed().length).toBeLessThanOrEqual(8);
		const last = revealed()[revealed().length - 1] as HTMLElement;
		expect(Number(last.style.getPropertyValue('--i'))).toBeLessThan(8);
		// a later tab change does not reveal again
		await view.rerender({ search: new URLSearchParams('type=tv') });
		await waitFor(() => expect(revealed().length).toBe(0));
	});

	it('does not animate under reduced motion', async () => {
		mql(true);
		mount();
		await screen.findByTestId(`discover-row-${hash(1)}`);
		expect(document.querySelectorAll('.dsc-reveal').length).toBe(0);
	});
});

describe('keyboard', () => {
	it('registers j and k in the discover scope and removes them on unmount', async () => {
		const view = mount();
		await screen.findByTestId('discover-newest-total');
		const mine = () => keys.list().filter((c) => c.scope === 'discover');
		expect(mine().map((c) => [c.id, c.keys])).toEqual(
			expect.arrayContaining([
				['list.next', ['j']],
				['list.previous', ['k']]
			])
		);
		view.unmount();
		expect(mine()).toHaveLength(0);
	});

	it('moves through the rows with j and k', async () => {
		const detach = keys.attach();
		try {
			mount();
			const first = (await screen.findByTestId(`discover-row-${hash(1)}`)).closest<HTMLElement>(
				'[role="option"]'
			)!;
			first.focus();
			await fireEvent.keyDown(first, { key: 'j' });
			await waitFor(() => expect(document.activeElement?.getAttribute('data-index')).toBe('1'));
			await fireEvent.keyDown(document.activeElement!, { key: 'k' });
			await waitFor(() => expect(document.activeElement?.getAttribute('data-index')).toBe('0'));
		} finally {
			detach();
		}
	});
});

describe('announcements', () => {
	it('announces the count only after a tab switch', async () => {
		const view = mount();
		await screen.findByTestId('discover-newest-total');
		expect(screen.getByTestId('discover-announce').textContent).toBe('');
		handler = () => response({ newest: newest({ hits: [hit(1)], total: 1284 }) });
		await view.rerender({ search: new URLSearchParams('type=movie') });
		await waitFor(() =>
			expect(screen.getByTestId('discover-announce').textContent).toBe('1,284 torrents in Movie')
		);
	});
});
