import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, within } from '@testing-library/svelte';
import { clearDetailCaches } from './cache';
import { torrentDetail } from './fixtures';
import DetailActions from './DetailActions.svelte';

const copyMagnet = vi.hoisted(() => vi.fn());
const sendHash = vi.hoisted(() => vi.fn());
const client = vi.hoisted(() => ({ name: null as string | null }));
const label = vi.hoisted(() => vi.fn());
vi.mock('./magnet', () => ({ copyMagnet }));
vi.mock('./send', () => ({
	sendLabel: async (fresh?: boolean) => (label(fresh), client.name && `Send to ${client.name}`),
	sendHash
}));

beforeEach(() => copyMagnet.mockClear());

it('copies only when an available magnet is activated', async () => {
	const detail = torrentDetail();
	render(DetailActions, { detail });
	expect(copyMagnet).not.toHaveBeenCalled();
	screen.getByTestId('detail-copy-magnet').click();
	await vi.dynamicImportSettled();
	expect(copyMagnet).toHaveBeenCalledExactlyOnceWith({ magnet: detail.magnet });
});

it('drops a copy whose lazy module resolves after visibility invalidation', async () => {
	render(DetailActions, { detail: torrentDetail() });
	screen.getByTestId('detail-copy-magnet').click();
	clearDetailCaches(false);
	await vi.dynamicImportSettled();
	expect(copyMagnet).not.toHaveBeenCalled();
});

it('drops a copy whose lazy module resolves after navigation destroys the view', async () => {
	const view = render(DetailActions, { detail: torrentDetail() });
	screen.getByTestId('detail-copy-magnet').click();
	view.unmount();
	await vi.dynamicImportSettled();
	expect(copyMagnet).not.toHaveBeenCalled();
});

describe('Send to the client', () => {
	it('is the primary action only when a client is configured; Copy magnet stays', async () => {
		client.name = null;
		const none = render(DetailActions, { detail: torrentDetail() });
		await vi.dynamicImportSettled();
		expect(screen.queryByTestId('detail-send')).toBeNull();
		none.unmount();

		client.name = 'qBittorrent';
		const detail = torrentDetail();
		render(DetailActions, { detail });
		const button = await screen.findByTestId('detail-send');
		expect(button.textContent?.trim()).toBe('Send to qBittorrent');
		expect(screen.getByTestId('detail-copy-magnet')).toBeTruthy();
		expect(label).toHaveBeenCalledWith(true); // re-read, not the session cache
		button.click();
		await vi.waitFor(() => expect(sendHash).toHaveBeenCalledExactlyOnceWith(detail.hash), {
			timeout: 10_000
		});
		client.name = null;
	});
});

describe('Open in torrent client and the actions menu', () => {
	it('links Open in torrent client only for a magnet: URL', () => {
		const { unmount } = render(DetailActions, { detail: torrentDetail() });
		const link = screen.getByTestId('detail-open-magnet');
		expect(link.getAttribute('href')).toMatch(/^magnet:\?/);
		expect(link.textContent).toBe('Open in torrent client');
		unmount();
		render(DetailActions, { detail: torrentDetail({ magnet: 'javascript:alert(1)' }) });
		expect(screen.queryByTestId('detail-open-magnet')).toBeNull();
	});

	it('puts Open full page in the menu for the drawer only, and Block in both', async () => {
		const { unmount } = render(DetailActions, { detail: torrentDetail(), mode: 'drawer' });
		await fireEvent.click(screen.getByTestId('detail-actions-menu'));
		expect(
			await screen.findByRole('menuitem', { name: 'Open full page' }, { timeout: 10_000 })
		).toBeTruthy();
		expect(screen.getByRole('menuitem', { name: 'Block this…' })).toBeTruthy();
		unmount();
		document.body.innerHTML = '';
		render(DetailActions, { detail: torrentDetail(), mode: 'page' });
		await fireEvent.click(screen.getByTestId('detail-actions-menu'));
		expect(
			await screen.findByRole('menuitem', { name: 'Block this…' }, { timeout: 10_000 })
		).toBeTruthy();
		expect(screen.queryByRole('menuitem', { name: 'Open full page' })).toBeNull();
	});
});

describe('the no-client hint', () => {
	afterEach(() => {
		client.name = null;
		localStorage.clear();
		vi.restoreAllMocks();
	});

	it('shows once per browser with a link to Set up, and not again', async () => {
		localStorage.clear();
		label.mockClear();
		const first = render(DetailActions, { detail: torrentDetail() });
		const hint = await screen.findByTestId('detail-client-hint');
		expect(hint.textContent).toContain('Set up');
		expect(within(hint).getByRole('link').getAttribute('href')).toBe('/settings/integrations');
		first.unmount();
		render(DetailActions, { detail: torrentDetail() });
		await vi.waitFor(() => expect(label).toHaveBeenCalledTimes(2), { timeout: 10_000 });
		await vi.dynamicImportSettled();
		expect(screen.queryByTestId('detail-client-hint')).toBeNull();
	});

	it('still renders, and shows the hint, when storage is unavailable', async () => {
		vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
			throw new Error('blocked');
		});
		vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
			throw new Error('blocked');
		});
		render(DetailActions, { detail: torrentDetail() });
		expect(await screen.findByTestId('detail-client-hint')).toBeTruthy();
	});

	it('shows no hint when a client is set up', async () => {
		client.name = 'qBittorrent';
		render(DetailActions, { detail: torrentDetail() });
		expect(await screen.findByTestId('detail-send')).toBeTruthy();
		expect(screen.queryByTestId('detail-client-hint')).toBeNull();
	});
});
