import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
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
beforeEach(() => get.mockReset());

describe('Parsed', () => {
	it('shows the title with its provenance and the type, then the spans from siblings', async () => {
		get.mockResolvedValue({ identity });
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
		get.mockResolvedValue({ identity: null });
		render(Parsed, { hash: HASH, hit: searchHit({ resolved_via: null }) });
		expect(screen.getByTestId('detail-parsed-provenance').textContent).toBe('From: name');
	});

	it('adds the adult type for an adult torrent, and shows an unknown type as sent', () => {
		get.mockResolvedValue({ identity: null });
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
		get.mockResolvedValue({ identity: { ...identity, absolute: [], years: [] } });
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
		get.mockResolvedValueOnce({ identity });
		await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
		await waitFor(() => expect(screen.getByTestId('detail-parsed-years')).toBeTruthy());
		for (const word of ['Resolution', 'Codec', 'Language', 'Group', 'Edition']) {
			expect(screen.queryByText(word)).toBeNull();
		}
	});

	it('uses singular labels for one plain value, plural for several or a range', async () => {
		get.mockResolvedValue({
			identity: { ...identity, years: ['2019'], seasons: ['1-2'] }
		});
		render(Parsed, { hash: HASH, hit: searchHit() });
		await waitFor(() => expect(screen.getByTestId('detail-parsed-years')).toBeTruthy());
		const label = (id: string) => screen.getByTestId(id).previousElementSibling?.textContent;
		expect(label('detail-parsed-years')).toBe('Year');
		expect(label('detail-parsed-seasons')).toBe('Seasons');
	});
});
