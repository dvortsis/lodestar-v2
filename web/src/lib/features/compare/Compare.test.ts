import { render, screen, waitFor, within, fireEvent } from '@testing-library/svelte';
import { tick } from 'svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import { toasts } from '$lib/ui/feedback/toasts.svelte';
import { fileList, HASHES, response } from './testing';
import { page, setUrl } from './testing-page.svelte';

const mocks = vi.hoisted(() => ({ post: vi.fn(), get: vi.fn(), goto: vi.fn() }));
vi.mock('$app/state', () => import('./testing-page.svelte'));
vi.mock('$app/navigation', () => ({ goto: mocks.goto, afterNavigate: () => {} }));
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { post: mocks.post, get: mocks.get }
}));

import Compare from './Compare.svelte';

function media(wide: boolean, wider = wide) {
	window.matchMedia = vi.fn((q: string) => ({
		matches: q.includes('1366') ? wider : wide,
		addEventListener() {},
		removeEventListener() {}
	})) as never;
}
const open = (n: number, extra = '') =>
	setUrl(`/compare?h=${HASHES.slice(0, n).join(',')}${extra}`);
const err = (status: number, code: string) => new ApiError(status, code, code, { field: 'hashes' });

beforeEach(() => {
	media(true);
	mocks.post.mockReset();
	mocks.get.mockReset().mockResolvedValue({ magnet: 'magnet:?xt=urn:btih:x' });
	mocks.goto.mockReset().mockImplementation(async (href: string) => void setUrl(href));
	toasts.items = [];
	setUrl('/compare');
});
afterEach(() => {
	vi.useRealTimers();
});

describe('choose state', () => {
	it('sends no request below two torrents and offers Search', async () => {
		open(1);
		render(Compare);
		expect(screen.getByRole('heading', { level: 1, name: 'Compare' })).toBeTruthy();
		expect(screen.getByText(/Select 2 to 5 torrents in Search/)).toBeTruthy();
		expect(screen.getByRole('link', { name: 'Go to Search' }).getAttribute('href')).toBe('/search');
		expect(mocks.post).not.toHaveBeenCalled();
	});
});

describe('columns layout', () => {
	beforeEach(() => {
		mocks.post.mockResolvedValue(response(3));
		open(3, '&density=compact');
	});

	it('posts the hashes in URL order', async () => {
		render(Compare);
		await screen.findByTestId('compare-table');
		expect(mocks.post).toHaveBeenCalledTimes(1);
		expect(mocks.post.mock.calls[0]![0]).toBe('/compare');
		expect(mocks.post.mock.calls[0]![1].body).toEqual({ hashes: HASHES.slice(0, 3) });
	});

	it('is a table with a caption, one column header per torrent and row headers', async () => {
		render(Compare);
		const table = await screen.findByTestId('compare-table');
		expect(within(table).getByText('Comparison of 3 torrents, one column each')).toBeTruthy();
		expect(within(table).getAllByRole('columnheader')).toHaveLength(4);
		expect(within(table).getByRole('rowheader', { name: /Resolution/ })).toBeTruthy();
		expect(screen.queryByTestId('compare-list')).toBeNull();
	});

	it('says "differs" with an icon only on rows the server flagged', async () => {
		render(Compare);
		await screen.findByTestId('compare-table');
		const flagged = screen
			.getAllByTestId('compare-differs')
			.map((el) => el.closest('tr')!.getAttribute('data-testid'));
		expect(flagged).toEqual(['compare-row-attr-res', 'compare-row-attr-group']);
		const badge = screen.getAllByTestId('compare-differs')[0]!;
		expect(badge.textContent).toBe('differs');
		expect(badge.querySelector('svg[aria-hidden="true"]')).toBeTruthy();
		for (const id of ['size', 'files', 'seeders', 'trackers', 'discovered']) {
			expect(screen.getByTestId(`compare-row-${id}`).textContent).not.toContain('differs');
		}
	});

	it('shows server numbers through the formatters and null as a dash with a name', async () => {
		mocks.post.mockResolvedValue(
			response(2, {
				torrents: [
					{ ...response(2).torrents[0]!, seeders: null, leechers: null, trackers_reporting: null },
					response(2).torrents[1]!
				]
			})
		);
		render(Compare);
		await screen.findByTestId('compare-table');
		const seeders = screen.getByTestId('compare-seeders-0');
		expect(seeders.textContent).toBe('—Never scraped');
		expect(seeders.querySelector('[aria-hidden="true"]')!.textContent).toBe('—');
		expect(screen.getByTestId('compare-seeders-1').textContent).toBe('30');
		expect(screen.getByTestId('compare-trackers-1').textContent).toBe('4 / 20');
		expect(screen.getByTestId('compare-size-1').textContent).toBe('3.73 GiB');
	});

	it("draws the composition bars on the server's shared scale, with text", async () => {
		render(Compare);
		await screen.findByTestId('compare-table');
		const data = response(3);
		const bar = screen.getByTestId('compare-composition-1-bar');
		const share = (data.torrents[1]!.size / data.scale_bytes) * 100;
		expect(parseFloat(bar.style.width)).toBeCloseTo(share, 5);
		expect(bar.getAttribute('aria-label')).toBe('80% Video, 20% Audio');
		expect(screen.getByTestId('compare-composition-1-dominant').textContent).toBe('80% Video');
		expect(parseFloat(screen.getByTestId('compare-composition-0-bar').style.width)).toBe(100);
	});

	it('says "Not classified yet" in place of an empty composition', async () => {
		const data = response(2);
		data.torrents[0]!.composition = [];
		mocks.post.mockResolvedValue(data);
		open(2);
		render(Compare);
		await screen.findByTestId('compare-table');
		expect(screen.getByTestId('compare-composition-0').textContent).toBe('Not classified yet');
	});

	it('uses the raw name as the heading when there is no title', async () => {
		const data = response(2);
		data.torrents[0]!.title = null;
		mocks.post.mockResolvedValue(data);
		open(2);
		render(Compare);
		const head = await screen.findByTestId('compare-column-0');
		expect(head.textContent).toContain(data.torrents[0]!.name);
		expect(head.querySelector('code')).toBeNull();
		expect(screen.getByTestId('compare-column-1').querySelector('code')).toBeTruthy();
	});

	it('says so when there are no parsed attributes', async () => {
		mocks.post.mockResolvedValue(response(2, { differs: {} }));
		open(2);
		render(Compare);
		await screen.findByTestId('compare-table');
		expect(screen.getByTestId('compare-no-attributes')).toBeTruthy();
	});

	it('moves four or five columns to the wider breakpoint', async () => {
		media(true, false);
		mocks.post.mockResolvedValue(response(4));
		open(4);
		render(Compare);
		await screen.findByTestId('compare-list');
		expect(screen.queryByTestId('compare-table')).toBeNull();
	});
});

describe('attribute-first list', () => {
	it('replaces the table below x-large, one section per attribute', async () => {
		media(false);
		mocks.post.mockResolvedValue(response(2));
		open(2);
		render(Compare);
		const list = await screen.findByTestId('compare-list');
		expect(screen.queryByTestId('compare-table')).toBeNull();
		const res = within(list).getByTestId('compare-row-attr-res');
		expect(
			within(res).getByRole('heading', { level: 2, name: /Resolution.*differs/ })
		).toBeTruthy();
		expect(res.textContent).toContain('A · The Lantern Keeper');
		expect(res.textContent).toContain('2160p');
		expect(within(list).getByTestId('compare-row-attr-year').textContent).not.toContain('differs');
		expect(
			within(list).getAllByRole('button', { name: /^Remove .* from comparison$/ })
		).toHaveLength(2);
		const bars = within(list).getAllByRole('img');
		expect(bars).toHaveLength(2);
	});
});

describe('file diff', () => {
	it('is collapsed, headed with the server count, and shortened wording uses count only', async () => {
		const data = response(2);
		data.file_diff.only[1] = fileList(12345, 3, true);
		data.file_diff.only[0] = fileList(1);
		mocks.post.mockResolvedValue(data);
		open(2);
		render(Compare);
		await screen.findByTestId('compare-table');
		expect(screen.getByTestId('compare-diff-only-1-count').textContent).toBe('12,345 files');
		expect(screen.getByTestId('compare-diff-only-0-count').textContent).toBe('1 file');
		expect(screen.getByTestId('compare-diff-common-count').textContent).toBe('2 files');
		const details = screen.getByTestId('compare-diff-only-1') as HTMLDetailsElement;
		expect(details.open).toBe(false);
		const truncated = screen.getByTestId('compare-diff-only-1-truncated');
		expect(truncated.textContent).toContain('List shortened: open the torrent to see every file');
		expect(truncated.querySelector('a')!.getAttribute('href')).toBe(`/t/${HASHES[1]}`);
		expect(screen.queryByTestId('compare-diff-only-0-truncated')).toBeNull();
		details.open = true;
		await fireEvent(details, new Event('toggle'));
		await waitFor(() => expect(within(details).getAllByRole('listitem')).toHaveLength(3));
	});

	it('names the rule for three or more torrents', async () => {
		mocks.post.mockResolvedValue(response(3));
		open(3);
		render(Compare);
		await screen.findByTestId('compare-table');
		expect(screen.getByText(/some, but not all, share aren't/)).toBeTruthy();
		expect(screen.getByTestId('compare-diff-common').textContent).toContain('In all');
	});
});

describe('errors', () => {
	const cases: [string, number, string][] = [
		['compare_count', 400, 'Compare needs 2 to 5 torrents'],
		['duplicate_hash', 400, 'more than once'],
		['bad_hash', 400, "One of these torrents isn't valid"]
	];
	for (const [code, status, text] of cases) {
		it(`${code}: says what is wrong and lists the torrents with a Remove each`, async () => {
			mocks.post.mockRejectedValue(err(status, code));
			open(3);
			render(Compare);
			const panel = await screen.findByTestId(`compare-error-${code}`);
			expect(panel.textContent).toContain(text);
			expect(within(panel).getAllByRole('button', { name: /^Remove / })).toHaveLength(3);
			expect(within(panel).getByRole('link', { name: 'Back to Search' })).toBeTruthy();
		});
	}

	it('torrent_not_found: names nothing, offers Remove and Back to Search', async () => {
		mocks.post.mockRejectedValue(new ApiError(404, 'torrent_not_found', 'x'));
		open(2);
		render(Compare);
		const panel = await screen.findByTestId('compare-error-not-found');
		expect(panel.textContent).toContain("One of these torrents isn't available");
		expect(within(panel).getAllByRole('button', { name: /^Remove / })).toHaveLength(2);
	});

	it('network and 5xx: a banner with Retry that asks again', async () => {
		mocks.post.mockRejectedValueOnce(new ApiError(0, 'network', 'offline'));
		open(2);
		render(Compare);
		const banner = await screen.findByRole('alert');
		expect(banner.textContent).toContain("Couldn't reach Lodestar");
		mocks.post.mockResolvedValue(response(2));
		await fireEvent.click(within(banner).getByRole('button', { name: 'Try again' }));
		await screen.findByTestId('compare-table');
		expect(mocks.post).toHaveBeenCalledTimes(2);
	});

	it('a 5xx on the first load is a banner with Retry too', async () => {
		mocks.post.mockRejectedValue(new ApiError(503, 'unavailable', 'x'));
		open(2);
		render(Compare);
		const banner = await screen.findByRole('alert');
		expect(banner.textContent).toContain("couldn't load");
		expect(within(banner).getByRole('button', { name: 'Try again' })).toBeTruthy();
	});

	it('keeps the last good comparison visible when the next request fails (stale)', async () => {
		mocks.post.mockResolvedValue(response(3));
		open(3);
		render(Compare);
		await screen.findByTestId('compare-table');
		mocks.post.mockRejectedValue(new ApiError(0, 'network', 'offline'));
		await fireEvent.click(
			screen.getAllByRole('button', { name: /^Remove .* from comparison$/ })[2]!
		);
		const banner = await screen.findByRole('status');
		expect(banner.textContent).toContain('Showing the last comparison that loaded');
		expect(screen.getByTestId('compare-table')).toBeTruthy();
		mocks.post.mockResolvedValue(response(2));
		await fireEvent.click(within(banner).getByRole('button', { name: 'Try again' }));
		await waitFor(() => expect(screen.queryByRole('status')).toBeNull());
		expect(mocks.post).toHaveBeenCalledTimes(3);
	});

	it('shows a skeleton only for answers slower than 300 ms', async () => {
		vi.useFakeTimers();
		let resolve!: (v: ReturnType<typeof response>) => void;
		mocks.post.mockReturnValue(new Promise((r) => (resolve = r)));
		open(2);
		render(Compare);
		await tick();
		expect(screen.getByTestId('compare-region').getAttribute('aria-busy')).toBe('true');
		expect(screen.queryByTestId('compare-skeleton')).toBeNull();
		await vi.advanceTimersByTimeAsync(301);
		expect(screen.getByTestId('compare-skeleton')).toBeTruthy();
		resolve(response(2));
		vi.useRealTimers();
		await screen.findByTestId('compare-table');
		expect(screen.queryByTestId('compare-skeleton')).toBeNull();
		await waitFor(() =>
			expect(screen.getByTestId('compare-region').getAttribute('aria-busy')).toBe('false')
		);
	});
});

describe('Remove', () => {
	it('asks the server again with the rest, pushes history and focuses a neighbour', async () => {
		mocks.post.mockImplementation(async (_p: string, o: { body: { hashes: string[] } }) => {
			const data = response(o.body.hashes.length);
			data.torrents.forEach((t, i) => (t.hash = o.body.hashes[i]!));
			return data;
		});
		open(3, '&density=compact');
		render(Compare);
		await screen.findByTestId('compare-table');
		const remove = screen.getAllByRole('button', { name: /^Remove .* from comparison$/ })[1]!;
		await fireEvent.click(remove);
		expect(mocks.goto).toHaveBeenCalledWith(
			`/compare?h=${HASHES[0]},${HASHES[2]}&density=compact`,
			{
				keepFocus: true,
				noScroll: true
			}
		);
		await waitFor(() => expect(mocks.post).toHaveBeenCalledTimes(2));
		expect(mocks.post.mock.calls[1]![1].body).toEqual({ hashes: [HASHES[0], HASHES[2]] });
		await waitFor(() => expect(screen.getAllByTestId(/^compare-column-/)).toHaveLength(2));
		await waitFor(() => {
			const column = document.querySelector(`[data-hash="${HASHES[2]}"]`)!;
			expect(document.activeElement).toBe(column.querySelector('[data-first-action]'));
		});
	});

	it('goes back to the choose state when one is left, and focuses the heading', async () => {
		mocks.post.mockResolvedValue(response(2));
		open(2);
		render(Compare);
		await screen.findByTestId('compare-table');
		await fireEvent.click(screen.getAllByRole('button', { name: /^Remove / })[0]!);
		await screen.findByText(/Select 2 to 5 torrents/);
		await waitFor(() =>
			expect(document.activeElement).toBe(screen.getByRole('heading', { level: 1 }))
		);
		expect(page.url.searchParams.get('h')).toBe(HASHES[1]);
	});
});

describe('Copy magnet', () => {
	const magnet = 'magnet:?xt=urn:btih:' + HASHES[0];
	beforeEach(() => {
		mocks.post.mockResolvedValue(response(2));
		mocks.get.mockResolvedValue({ magnet });
		open(2);
	});
	const press = async () => {
		render(Compare);
		await screen.findByTestId('compare-table');
		await fireEvent.click(screen.getAllByRole('button', { name: /^Copy magnet for/ })[0]!);
	};
	const clipboard = (writeText?: unknown) =>
		Object.defineProperty(navigator, 'clipboard', {
			value: writeText && { writeText },
			configurable: true
		});

	it("copies the server's magnet and toasts", async () => {
		const writeText = vi.fn().mockResolvedValue(undefined);
		clipboard(writeText);
		await press();
		await waitFor(() => expect(writeText).toHaveBeenCalledWith(magnet));
		expect(mocks.get.mock.calls[0]![1]).toEqual({ path: { hash: HASHES[0] } });
		await waitFor(() => expect(toasts.items.map((t) => t.text)).toContain('Magnet copied.'));
	});

	it('fetches the magnet on hover or focus, once', async () => {
		clipboard(vi.fn().mockResolvedValue(undefined));
		render(Compare);
		await screen.findByTestId('compare-table');
		const group = document.querySelector(`[data-hash="${HASHES[0]}"]`)!;
		await fireEvent.pointerEnter(group);
		await fireEvent.focusIn(group);
		expect(mocks.get).toHaveBeenCalledTimes(1);
	});

	it('shows the magnet in a selected read-only field when no copy method works', async () => {
		clipboard(undefined);
		Object.defineProperty(document, 'execCommand', { value: () => false, configurable: true });
		await press();
		const field = (await screen.findByTestId('compare-manual-magnet')) as HTMLInputElement;
		await waitFor(() => expect(field.value).toBe(magnet));
		expect(field.readOnly).toBe(true);
		expect(document.activeElement).toBe(field);
		expect(screen.getByText(/Press (Ctrl\+C|⌘C) to copy/)).toBeTruthy();
	});

	it('toasts an error when the magnet cannot be fetched', async () => {
		mocks.get.mockRejectedValue(new ApiError(0, 'network', 'x'));
		await press();
		await waitFor(() => expect(toasts.items[0]?.kind).toBe('error'));
	});
});
