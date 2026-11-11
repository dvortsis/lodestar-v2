// NeedChip's re-check loop, on its own (CMP-9b): it stops at a minute's worth of reads, and it stops
// when the chip goes, including a read that is still in flight. Real timers with a tiny interval
// wait on conditions; the unmount tests drive fake timers and count calls.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { mockFetch } from '../filters/flows/testing/fetch';
import NeedChip from './NeedChip.svelte';

configure({ defaultHidden: true, asyncUtilTimeout: 10_000 });
vi.setConfig({ testTimeout: 30_000 });

const OFFER = {
	id: 'qbittorrent',
	name: 'qBittorrent',
	version: '5.0.4',
	licence: 'GPL-2.0-or-later',
	size_bytes: 41_943_040,
	sha256: 'd'.repeat(64),
	replaces: null
};
const need = { companion: 'qbittorrent', state: 'missing' } as const;

function setup(props: Record<string, unknown>) {
	const net = mockFetch();
	net.on('GET /companions/qbittorrent/install', { body: OFFER });
	net.on('POST /companions/qbittorrent/install', { body: { id: 'qbittorrent' } });
	return { net, view: render(NeedChip, { need, recheckMs: 5, ...props }) };
}
const install = async () => {
	await fireEvent.click(await screen.findByTestId('companion-need-setup'));
	await fireEvent.click(
		within(await screen.findByRole('dialog')).getByRole('button', { name: 'Install' })
	);
};
afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

describe('the re-check after an install', () => {
	it('reads at most 21 times (the install, then a minute of re-reads), then gives Set up back', async () => {
		const oninstalled = vi.fn();
		setup({ oninstalled });
		await install();
		await waitFor(() => expect(oninstalled).toHaveBeenCalledTimes(21));
		// Without the cap the loop never ends and Set up never returns.
		await screen.findByTestId('companion-need-setup');
		expect(screen.getByTestId('companion-need-text').textContent).toBe('Needs qBittorrent');
	});

	it('stops when the chip goes while a read is waiting to start', async () => {
		// Fake timers from the start, so the pending read's timer is one the test controls.
		vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
		const oninstalled = vi.fn();
		const { view } = setup({ oninstalled, recheckMs: 50 });
		await install();
		await waitFor(() => expect(oninstalled).toHaveBeenCalledTimes(1));
		view.unmount();
		await vi.advanceTimersByTimeAsync(10_000);
		expect(oninstalled).toHaveBeenCalledTimes(1);
	});

	it('stops when the chip goes while a read is in flight', async () => {
		let release!: () => void;
		let calls = 0;
		const oninstalled = vi.fn(() =>
			++calls < 2 ? undefined : new Promise<void>((resolve) => (release = resolve))
		);
		const { view } = setup({ oninstalled });
		await install();
		await waitFor(() => expect(oninstalled).toHaveBeenCalledTimes(2)); // the second read hangs
		vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
		view.unmount();
		release();
		await vi.advanceTimersByTimeAsync(10_000);
		expect(oninstalled).toHaveBeenCalledTimes(2);
	});
});
