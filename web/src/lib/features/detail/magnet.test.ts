import { beforeEach, describe, expect, it, vi } from 'vitest';
import { toasts } from '$lib/ui/feedback/toasts.svelte';
import type { TorrentDetail } from './cache';

const copyText = vi.fn();
const get = vi.fn();
vi.mock('$lib/util/clipboard', () => ({ copyText }));
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get }
}));

const { copyMagnet, openMagnet, magnetFor } = await import('./magnet');
const { clearDetailCaches } = await import('./cache');
const { manual } = await import('./manual.svelte');

const HASH = 'c'.repeat(40);
const MAGNET = `magnet:?xt=urn:btih:${HASH}`;
const detail = (magnet: string) => ({ hash: HASH, magnet }) as TorrentDetail;

beforeEach(() => {
	copyText.mockReset();
	get.mockReset();
	clearDetailCaches();
	manual.magnet = null;
	toasts.items = [];
});

describe('copyMagnet', () => {
	it('toasts on a copy', async () => {
		copyText.mockResolvedValue('copied');
		expect(await copyMagnet({ magnet: MAGNET })).toBe('copied');
		expect(copyText).toHaveBeenCalledWith(MAGNET);
		expect(toasts.items.map((t) => [t.kind, t.text])).toEqual([['success', 'Magnet copied']]);
		expect(manual.magnet).toBeNull();
	});

	it('opens the manual field when neither method worked', async () => {
		copyText.mockResolvedValue('manual');
		expect(await copyMagnet({ magnet: MAGNET })).toBe('manual');
		expect(manual.magnet).toBe(MAGNET);
		// mounted on demand, with no drawer or page around it
		const field = await vi.waitFor(
			() => {
				const el = document.querySelector<HTMLInputElement>('[data-testid="detail-magnet-field"]');
				if (!el) throw new Error('not mounted yet');
				return el;
			},
			{ timeout: 10_000 }
		);
		expect(field.value).toBe(MAGNET);
		expect(toasts.items).toEqual([]);
	});

	it('refuses anything that is not a magnet link', async () => {
		expect(await copyMagnet({ magnet: 'javascript:alert(1)' })).toBe('failed');
		expect(copyText).not.toHaveBeenCalled();
		expect(toasts.items[0]!.kind).toBe('error');
	});

	it('fetches the magnet for a hash (a row without a drawer), once', async () => {
		get.mockResolvedValue(detail(MAGNET));
		copyText.mockResolvedValue('copied');
		await copyMagnet({ hash: HASH });
		await copyMagnet({ hash: HASH });
		expect(get).toHaveBeenCalledTimes(1);
		expect(copyText).toHaveBeenLastCalledWith(MAGNET);
	});

	it('reports a failed lookup', async () => {
		get.mockRejectedValue(new Error('offline'));
		expect(await copyMagnet({ hash: HASH })).toBe('failed');
		expect(toasts.items[0]!.text).toBe("Couldn't get the magnet link.");
	});
});

describe('magnetFor', () => {
	it('drops a body whose magnet has the wrong scheme', async () => {
		get.mockResolvedValue(detail('http://evil.example/'));
		expect(await magnetFor(HASH)).toBeNull();
	});
});

describe('openMagnet', () => {
	it('follows a checked magnet as a link and removes the anchor', async () => {
		const click = vi.fn();
		const create = document.createElement.bind(document);
		vi.spyOn(document, 'createElement').mockImplementation((tag: string) => {
			const el = create(tag);
			if (tag === 'a') el.click = click;
			return el;
		});
		expect(await openMagnet({ magnet: MAGNET })).toBe(true);
		expect(click).toHaveBeenCalledTimes(1);
		expect(document.querySelector('a[href^="magnet:"]')).toBeNull();
		vi.restoreAllMocks();
	});

	it('does not follow a bad scheme', async () => {
		expect(await openMagnet({ magnet: 'javascript:alert(1)' })).toBe(false);
		expect(toasts.items[0]!.kind).toBe('error');
	});
});
