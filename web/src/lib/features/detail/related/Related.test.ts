import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { ApiError } from '$lib/api/client';
import { clearDetailCaches } from '../cache';
import { HASH, searchHit, torrentDetail } from '../fixtures';
import type { HitGroup, Siblings } from './lists';

vi.setConfig({ testTimeout: 20_000 });
const WAIT = { timeout: 10_000 };

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get }
}));
// The drawer's open function, as DetailHostBody hands it down (none outside the drawer).
const openDetail = vi.fn();
const provided = { open: true };

const { default: Related } = await import('./Related.svelte');

const hex = (c: string) => c.repeat(40);
const group = (hits: ReturnType<typeof searchHit>[] = [], truncated = false): HitGroup => ({
	hits,
	reason: null,
	status: 'available',
	truncated
});
const siblings = (over: Partial<Siblings> = {}): Siblings => ({
	hash: HASH,
	identical_payload: group(),
	identical_payload_partial: false,
	identity: null,
	other_versions: group(),
	same_file: group(),
	same_file_capped: false,
	same_file_partial: false,
	similar_payload: group(),
	similar_payload_partial: false,
	...over
});
const full = () =>
	siblings({
		other_versions: group(
			[
				searchHit({
					hash: hex('a'),
					name: 'Show.S01.2160p.WEB-DL-GRP',
					resolution: '2160p',
					size: 9e9
				}),
				searchHit({ hash: hex('b'), name: 'Show.S01.720p.HDTV-OTH', resolution: '720p' })
			],
			true
		),
		identical_payload: group([searchHit({ hash: hex('c'), name: 'Show.S01.repost', seeders: 3 })]),
		same_file: {
			hits: [],
			reason: 'needs_v2_files',
			status: 'unavailable',
			truncated: false
		},
		similar_payload_partial: true
	});

const show = (mode: 'drawer' | 'page' = 'drawer') =>
	render(Related, {
		props: { detail: torrentDetail(), hit: searchHit({ resolution: '1080p' }), mode },
		context: new Map(provided.open ? [['lodestar.detail.open', openDetail]] : [])
	});
const list = (key: string) =>
	screen.getAllByTestId('detail-related-list').find((el) => el.dataset.list === key) as HTMLElement;
const text = (el: Element) => el.textContent!.replace(/[\s\u00a0]+/g, ' ').trim();

beforeEach(() => {
	get.mockReset();
	openDetail.mockReset();
	provided.open = true;
	clearDetailCaches(false); // the siblings answer is kept: nothing carries into the next test
});

describe('Related', () => {
	it('asks for the open torrent and shows four labelled lists with counts', async () => {
		get.mockResolvedValue(full());
		show();
		await waitFor(() => expect(screen.getAllByTestId('detail-related-list')).toHaveLength(4), WAIT);
		expect(get).toHaveBeenCalledWith(
			'/torrents/{hash}/siblings',
			expect.objectContaining({ path: { hash: HASH } })
		);
		expect(screen.getAllByTestId('detail-related-list-h').map(text)).toEqual([
			'Other versions 2+',
			'Identical clones 1',
			'Same file',
			'Similar payloads 0'
		]);
		expect(text(within(list('same_file')).getByTestId('detail-related-reason'))).toBe(
			'Only v2 and hybrid torrents can be matched by file.'
		);
		expect(text(within(list('similar_payload')).getByTestId('detail-related-none'))).toBe(
			'None found.'
		);
		expect(within(list('similar_payload')).getByTestId('detail-related-gap')).toBeTruthy();
	});

	it('shows on each row only the attributes that differ', async () => {
		get.mockResolvedValue(full());
		show();
		await waitFor(() => expect(screen.getAllByTestId('detail-related-row')).toHaveLength(3), WAIT);
		const [uhd, hd, clone] = screen.getAllByTestId('detail-related-row');
		expect(text(within(uhd!).getByTestId('detail-related-differs'))).toBe('2160p · 8.38 GiB');
		expect(text(within(hd!).getByTestId('detail-related-differs'))).toBe('720p');
		expect(text(within(clone!).getByTestId('detail-related-differs'))).toBe('3 seeders');
	});

	it('Compare these needs 1 to 4 picks and links /compare with the open torrent first', async () => {
		get.mockResolvedValue(full());
		show();
		await waitFor(() => expect(screen.getAllByTestId('detail-related-pick')).toHaveLength(3), WAIT);
		const compare = () => screen.getByTestId('detail-related-compare');
		expect(compare().tagName).toBe('BUTTON');
		expect((compare() as HTMLButtonElement).disabled).toBe(true);
		expect(text(screen.getByTestId('detail-related-why'))).toBe(
			'Pick 1 to 4 torrents to compare with this one.'
		);
		const [p1, , p3] = screen.getAllByTestId('detail-related-pick');
		await fireEvent.click(p3!);
		await fireEvent.click(p1!);
		expect(compare().tagName).toBe('A');
		expect(compare().getAttribute('href')).toBe(`/compare?h=${HASH},${hex('c')},${hex('a')}`);
		expect(text(screen.getByTestId('detail-related-why'))).toBe('2 picked');
		await fireEvent.click(p3!);
		expect(compare().getAttribute('href')).toBe(`/compare?h=${HASH},${hex('a')}`);
	});

	it('refuses more than 5 torrents, saying how many to unpick', async () => {
		const hits = ['a', 'b', 'c', 'd', 'e', 'f'].map((c) => searchHit({ hash: hex(c), name: c }));
		get.mockResolvedValue(siblings({ other_versions: group(hits) }));
		show();
		await waitFor(() => expect(screen.getAllByTestId('detail-related-pick')).toHaveLength(6), WAIT);
		const boxes = screen.getAllByTestId('detail-related-pick');
		for (const box of boxes.slice(0, 4)) await fireEvent.click(box);
		// The reason Compare stops is spoken: the same polite live region carries the count and the
		// reason, so it stays in the page across the change.
		const why = screen.getByTestId('detail-related-why');
		expect(why.getAttribute('aria-live')).toBe('polite');
		expect(text(why)).toBe('4 picked');
		for (const box of boxes.slice(4)) await fireEvent.click(box);
		expect((screen.getByTestId('detail-related-compare') as HTMLButtonElement).disabled).toBe(true);
		expect(screen.getByTestId('detail-related-why')).toBe(why);
		expect(text(why)).toBe('Compare shows at most 5 torrents: unpick 2.');
	});

	it('opens a row in the drawer, and leaves it a plain link on the page', async () => {
		get.mockResolvedValue(full());
		const { unmount } = show('drawer');
		await waitFor(() => expect(screen.getAllByTestId('detail-related-name')).toHaveLength(3), WAIT);
		const name = screen.getAllByTestId('detail-related-name')[0]!;
		expect(name.getAttribute('href')).toBe(`/t/${hex('a')}`);
		await fireEvent.click(name);
		await waitFor(
			() =>
				expect(openDetail).toHaveBeenCalledWith(
					hex('a'),
					expect.objectContaining({ hash: hex('a') })
				),
			WAIT
		);
		unmount();
		openDetail.mockReset();
		show('page');
		await waitFor(() => expect(screen.getAllByTestId('detail-related-name')).toHaveLength(3), WAIT);
		const link = screen.getAllByTestId('detail-related-name')[0]!;
		// jsdom can't navigate: stop the link's default after the component has seen the click
		link.addEventListener('click', (e) => e.preventDefault());
		await fireEvent.click(link);
		expect(openDetail).not.toHaveBeenCalled();
	});

	it('follows the link when nothing hands it an open function', async () => {
		provided.open = false;
		get.mockResolvedValue(full());
		show('drawer');
		await waitFor(() => expect(screen.getAllByTestId('detail-related-name')).toHaveLength(3), WAIT);
		const link = screen.getAllByTestId('detail-related-name')[0]!;
		// Read last, after the component's handler, then stopped there: jsdom can't navigate.
		let prevented: boolean | undefined;
		const last = (e: Event) => {
			prevented = e.defaultPrevented;
			e.preventDefault();
		};
		window.addEventListener('click', last);
		try {
			await fireEvent.click(link);
		} finally {
			window.removeEventListener('click', last);
		}
		expect(prevented).toBe(false);
		expect(openDetail).not.toHaveBeenCalled();
	});

	it('keeps the ≈ on estimated seeders', async () => {
		get.mockResolvedValue(
			siblings({
				identical_payload: group([
					searchHit({ hash: hex('c'), seeders: 2400, seeders_approx: true })
				])
			})
		);
		show();
		const seeders = await screen.findByTestId('detail-related-seeders', {}, WAIT);
		expect(text(seeders)).toBe('≈ 2,400 seeders');
	});

	it('says why a capped or index-bound list may be short, and hides Compare with nothing to pick', async () => {
		get.mockResolvedValue(
			siblings({
				same_file_capped: true,
				similar_payload: {
					hits: [],
					reason: 'needs_group_minhash',
					status: 'unavailable',
					truncated: false
				}
			})
		);
		show();
		await waitFor(() => expect(screen.getAllByTestId('detail-related-list')).toHaveLength(4), WAIT);
		expect(text(within(list('same_file')).getByTestId('detail-related-gap'))).toBe(
			"Only this torrent's largest files were looked up, so some may be missing."
		);
		expect(text(within(list('similar_payload')).getByTestId('detail-related-reason'))).toBe(
			'Not available until the search index is rebuilt.'
		);
		expect(screen.queryByTestId('detail-related-compare')).toBeNull();
	});

	// The drawer's host, with the title Show all's open leaves focus on, and the ask it records.
	function host(ask: string | null) {
		const el = document.body.appendChild(document.createElement('div'));
		el.className = 'ls-detail-host';
		el.dataset.testid = 'drawer';
		if (ask) el.dataset.ask = ask;
		const title = el.appendChild(document.createElement('h2'));
		title.tabIndex = -1;
		title.focus();
		const mount = el.appendChild(document.createElement('div'));
		return { el, title, mount };
	}
	const showIn = (target: HTMLElement) =>
		render(Related, {
			target,
			props: { detail: torrentDetail(), hit: searchHit(), mode: 'drawer' },
			context: new Map([['lodestar.detail.open', openDetail]])
		});

	it('takes focus on mount when Show all asked for it, and asks once', async () => {
		get.mockResolvedValue(full());
		const h = host(HASH);
		showIn(h.mount);
		await waitFor(() => expect(document.activeElement?.textContent?.trim()).toBe('Related'), WAIT);
		expect(h.el.dataset.ask).toBeUndefined();
	});

	it('leaves focus alone when the reader moved on, or the ask was for another torrent', async () => {
		get.mockResolvedValue(full());
		const moved = host(HASH);
		const elsewhere = document.body.appendChild(document.createElement('button'));
		elsewhere.focus();
		showIn(moved.mount);
		await screen.findAllByTestId('detail-related-row', {}, WAIT);
		expect(document.activeElement).toBe(elsewhere);
		const other = host(hex('e'));
		showIn(other.mount);
		await waitFor(
			() => expect(screen.getAllByTestId('detail-related-row').length).toBeGreaterThan(0),
			WAIT
		);
		expect(document.activeElement).toBe(other.title);
	});

	it('moves the focus it holds to the drawer title when the answer is withheld', async () => {
		let reject!: (e: unknown) => void;
		get.mockReturnValue(new Promise((_, r) => (reject = r)));
		const h = host(HASH);
		showIn(h.mount);
		await waitFor(() => expect(document.activeElement?.textContent?.trim()).toBe('Related'), WAIT);
		reject(new ApiError(404, 'torrent_not_found', ''));
		await waitFor(() => expect(document.activeElement).toBe(h.title), WAIT);
	});

	it('shows nothing at all when the API withholds the torrent', async () => {
		get.mockRejectedValue(new ApiError(404, 'torrent_not_found', ''));
		show();
		await waitFor(() => expect(screen.queryByTestId('detail-related')).toBeNull(), WAIT);
		expect(screen.queryByTestId('detail-related-row')).toBeNull();
	});

	it('says when the load failed and retries on request', async () => {
		get.mockRejectedValueOnce(new ApiError(500, 'internal', '')).mockResolvedValueOnce(full());
		show();
		const failed = await screen.findByTestId('detail-related-failed', {}, WAIT);
		expect(text(failed)).toContain("Couldn't load related torrents.");
		await fireEvent.click(within(failed).getByRole('button', { name: 'Retry' }));
		// the button is gone: focus stays in the section, on its heading
		expect(document.activeElement?.textContent?.trim()).toBe('Related');
		await waitFor(() => expect(screen.getAllByTestId('detail-related-row')).toHaveLength(3), WAIT);
		expect(get).toHaveBeenCalledTimes(2);
	});

	it('ignores an answer about another torrent', async () => {
		get.mockResolvedValue(siblings({ hash: hex('f') }));
		show();
		await screen.findByTestId('detail-related-failed', {}, WAIT);
	});
});
