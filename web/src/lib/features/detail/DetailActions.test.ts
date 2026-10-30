import { beforeEach, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { clearDetailCaches } from './cache';
import { torrentDetail } from './fixtures';
import DetailActions from './DetailActions.svelte';

const copyMagnet = vi.hoisted(() => vi.fn());
vi.mock('./magnet', () => ({ copyMagnet }));

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
