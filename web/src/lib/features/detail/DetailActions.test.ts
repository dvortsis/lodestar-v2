import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { clearDetailCaches } from './cache';
import { torrentDetail } from './fixtures';
import DetailActions from './DetailActions.svelte';

const copyMagnet = vi.hoisted(() => vi.fn());
const sendHash = vi.hoisted(() => vi.fn());
const client = vi.hoisted(() => ({ name: null as string | null }));
vi.mock('./magnet', () => ({ copyMagnet }));
vi.mock('./send', () => ({
	sendLabel: async () => client.name && `Send to ${client.name}`,
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
		button.click();
		await vi.waitFor(() => expect(sendHash).toHaveBeenCalledExactlyOnceWith(detail.hash), {
			timeout: 10_000
		});
		client.name = null;
	});
});
