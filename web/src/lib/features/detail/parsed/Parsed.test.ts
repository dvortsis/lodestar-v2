import { beforeEach, describe, expect, it, vi } from 'vitest';
import { tick } from 'svelte';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { ApiError } from '$lib/api/client';
import { clearDetailCaches } from '../cache';
import { HASH, searchHit } from '../fixtures';

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get }
}));
const { default: Parsed } = await import('./Parsed.svelte');

const identity = {
	absolute: ['101-110'],
	episodes: ['1-10'],
	seasons: ['2'],
	title_words: ['show'],
	years: ['2019', '2020-2021']
};
beforeEach(() => {
	get.mockReset();
	clearDetailCaches(false); // the siblings answer is kept: nothing carries into the next test
});

describe('Parsed', () => {
	it('shows the title with its provenance and the type, then the spans from siblings', async () => {
		get.mockResolvedValue({ hash: HASH, identity });
		render(Parsed, { hash: HASH, hit: searchHit({ resolved_via: 'folder', adult_kind: null }) });
		expect(screen.getByTestId('detail-parsed-title').textContent).toContain('Show');
		expect(screen.getByTestId('detail-parsed-provenance').textContent).toBe('Resolved via folder');
		expect(screen.getByTestId('detail-parsed-type').textContent?.trim()).toBe('TV');
		await waitFor(() => expect(screen.getByTestId('detail-parsed-years')).toBeTruthy());
		expect(get).toHaveBeenCalledWith('/torrents/{hash}/siblings', {
			path: { hash: HASH },
			signal: expect.any(AbortSignal)
		});
		expect(screen.getByTestId('detail-parsed-years').textContent).toBe('2019, 2020-2021');
		expect(screen.getByTestId('detail-parsed-seasons').textContent).toBe('2');
		expect(screen.getByTestId('detail-parsed-episodes').textContent).toBe('1-10');
		expect(screen.getByTestId('detail-parsed-absolute').textContent).toBe('101-110');
	});

	it('says "From: name" when nothing was resolved', () => {
		get.mockResolvedValue({ hash: HASH, identity: null });
		render(Parsed, { hash: HASH, hit: searchHit({ resolved_via: null }) });
		expect(screen.getByTestId('detail-parsed-provenance').textContent).toBe('From: name');
	});

	it('adds the adult type for an adult torrent, and shows an unknown type as sent', () => {
		get.mockResolvedValue({ hash: HASH, identity: null });
		const { unmount } = render(Parsed, {
			hash: HASH,
			hit: searchHit({ content_type: 'adult', adult_kind: 'vr' })
		});
		expect(screen.getByTestId('detail-parsed-type').textContent?.trim()).toBe('Adult, VR');
		unmount();
		render(Parsed, { hash: HASH, hit: searchHit({ content_type: 'hologram' }) });
		expect(screen.getByTestId('detail-parsed-type').textContent?.trim()).toBe('hologram');
	});

	it('leaves out spans the server did not send, and shows "Not parsed" without a hit', async () => {
		get.mockResolvedValue({ hash: HASH, identity: { ...identity, absolute: [], years: [] } });
		render(Parsed, { hash: HASH, hit: null });
		expect(screen.getByTestId('detail-parsed-title').textContent?.trim()).toBe('Not parsed');
		await waitFor(() => expect(screen.getByTestId('detail-parsed-seasons')).toBeTruthy());
		expect(screen.queryByTestId('detail-parsed-years')).toBeNull();
		expect(screen.queryByTestId('detail-parsed-absolute')).toBeNull();
	});

	it('offers Retry when siblings fail, and does not invent release fields', async () => {
		get.mockRejectedValueOnce(new Error('offline'));
		render(Parsed, { hash: HASH, hit: searchHit() });
		expect(await screen.findByText("Couldn't load the parsed details.")).toBeTruthy();
		get.mockResolvedValueOnce({ hash: HASH, identity });
		await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
		await waitFor(() => expect(screen.getByTestId('detail-parsed-years')).toBeTruthy());
		for (const word of ['Resolution', 'Codec', 'Language', 'Group', 'Edition']) {
			expect(screen.queryByText(word)).toBeNull();
		}
	});

	it('shows no spans, and no error, when the API withholds the torrent', async () => {
		get.mockRejectedValue(new ApiError(404, 'torrent_not_found', ''));
		render(Parsed, { hash: HASH, hit: searchHit() });
		await waitFor(() => expect(get).toHaveBeenCalled(), { timeout: 10_000 });
		// The 404 has been delivered; the section reacts to it on the next flush.
		await expect(get.mock.results[0]!.value).rejects.toBeInstanceOf(ApiError);
		await tick();
		expect(screen.queryByTestId('detail-parsed-years')).toBeNull();
		expect(screen.queryByText("Couldn't load the parsed details.")).toBeNull();
		expect(screen.queryByRole('button', { name: 'Retry' })).toBeNull();
	});

	it('uses singular labels for one plain value, plural for several or a range', async () => {
		get.mockResolvedValue({
			hash: HASH,
			identity: { ...identity, years: ['2019'], seasons: ['1-2'] }
		});
		render(Parsed, { hash: HASH, hit: searchHit() });
		await waitFor(() => expect(screen.getByTestId('detail-parsed-years')).toBeTruthy());
		const label = (id: string) => screen.getByTestId(id).previousElementSibling?.textContent;
		expect(label('detail-parsed-years')).toBe('Year');
		expect(label('detail-parsed-seasons')).toBe('Seasons');
	});
});

describe('Parsed: the Release list', () => {
	const attrs = {
		res: '1080p',
		vcodec: 'h265',
		bits: '10',
		lang: 'en, multi',
		subs: 'fr',
		pack: 'yes',
		service: 'hidi',
		source: 'web-dl',
		year: '2008',
		season: '2',
		mystery: 'zzq'
	};
	const text = (id: string) => screen.getByTestId(id).textContent?.trim();

	it('labels each key as Compare does, formats values, and drops unknown keys', async () => {
		get.mockResolvedValue({ hash: HASH, identity: null });
		render(Parsed, { hash: HASH, hit: searchHit(), attributes: attrs });
		await waitFor(() => expect(screen.getByTestId('detail-release-year')).toBeTruthy());
		expect(text('detail-release-res')).toBe('1080p');
		expect(text('detail-release-vcodec')).toBe('H.265 (HEVC)');
		expect(text('detail-release-source')).toBe('WEB-DL');
		expect(text('detail-release-bits')).toBe('10-bit');
		expect(text('detail-release-lang')).toBe('English, Multiple languages');
		expect(text('detail-release-subs')).toBe('French');
		expect(text('detail-release-pack')).toBe('Season or collection pack');
		expect(text('detail-release-service')).toBe('HIDIVE');
		expect(text('detail-release-year')).toBe('2008');
		expect(text('detail-release-season')).toBe('2');
		const list = screen.getByTestId('detail-release');
		expect(list.textContent).toContain('Video codec');
		expect(list.textContent).toContain('Subtitles');
		expect(list.textContent).not.toContain('mystery');
		expect(list.textContent).not.toContain('zzq');
	});

	it('holds the identity keys back while the identity loads, then leaves out those it shows', async () => {
		let answer!: (v: unknown) => void;
		get.mockReturnValue(new Promise((r) => (answer = r)));
		render(Parsed, { hash: HASH, hit: searchHit(), attributes: { year: '2019', res: '720p' } });
		expect(text('detail-release-res')).toBe('720p');
		expect(screen.queryByTestId('detail-release-year')).toBeNull();
		answer({ hash: HASH, identity });
		await waitFor(() => expect(screen.getByTestId('detail-parsed-years')).toBeTruthy());
		expect(screen.queryByTestId('detail-release-year')).toBeNull();
		expect(text('detail-release-res')).toBe('720p');
	});

	it("shows this torrent's own year when the identity has none", async () => {
		get.mockResolvedValue({ hash: HASH, identity: null });
		render(Parsed, { hash: HASH, hit: searchHit(), attributes: { year: '2019' } });
		await waitFor(() => expect(screen.getByTestId('detail-release-year')).toBeTruthy());
		expect(text('detail-release-year')).toBe('2019');
	});

	it('shows nothing for null or {}', () => {
		get.mockResolvedValue({ hash: HASH, identity: null });
		const { unmount } = render(Parsed, { hash: HASH, hit: searchHit(), attributes: null });
		expect(screen.queryByTestId('detail-release')).toBeNull();
		unmount();
		render(Parsed, { hash: HASH, hit: searchHit(), attributes: {} });
		expect(screen.queryByTestId('detail-release')).toBeNull();
	});

	it('shows the parsed title only when it differs from the resolved one', () => {
		get.mockResolvedValue({ hash: HASH, identity: null });
		const hit = searchHit();
		const { unmount } = render(Parsed, { hash: HASH, hit, attributes: { title: hit.title ?? '' } });
		expect(screen.queryByTestId('detail-release')).toBeNull();
		unmount();
		render(Parsed, { hash: HASH, hit, attributes: { title: 'Another Name' } });
		expect(text('detail-release-title')).toBe('Another Name');
	});

	it('words the values in the UI language', async () => {
		const { applyLocale } = await import('$lib/i18n/locale.svelte');
		applyLocale('zh');
		try {
			get.mockResolvedValue({ hash: HASH, identity: null });
			render(Parsed, { hash: HASH, hit: searchHit(), attributes: { bits: '10', lang: 'fr' } });
			expect(text('detail-release-bits')).toBe('10 位');
			expect(text('detail-release-lang')).toBe('法语');
		} finally {
			applyLocale('en');
		}
	});
});
