import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';

const get = vi.hoisted(() => vi.fn());
const patch = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { get, patch }
}));
vi.mock('$app/paths', () => ({ resolve: (p: string) => p.replace('[section]', 'insights') }));

import PrivacyBanner from './PrivacyBanner.svelte';

const NOW = 1_700_000_000;
const off = { identifying_until: null, query_logging: false };

function kinds(until: number) {
	return [
		{ kind: 'hash', identifying: true, opt_in: false, until },
		{ kind: 'size', identifying: false, opt_in: false, until: -1 },
		{ kind: 'peer_activity', identifying: true, opt_in: true, until: 0 }
	];
}

beforeEach(() => {
	get.mockReset();
	patch.mockReset();
	vi.useFakeTimers({ toFake: ['Date', 'setInterval', 'clearInterval'] });
	vi.setSystemTime(NOW * 1000);
});
afterEach(() => vi.useRealTimers());

describe('PrivacyBanner', () => {
	it('names query logging and links to Settings → Insights', () => {
		render(PrivacyBanner, {
			overview: { privacy: { ...off, query_logging: true }, updated_at: NOW },
			reload: () => {}
		});
		expect(screen.getByText('Search queries are being saved')).toBeTruthy();
		const link = screen.getByRole('link', { name: /Settings → Insights/ });
		expect(link.getAttribute('href')).toBe('/settings/insights');
		expect(screen.queryByRole('button', { name: 'Turn off now' })).toBeNull();
		expect(screen.getAllByRole('status')).toHaveLength(1);
	});

	it('counts down on the server clock and hides at expiry, asking the server again', async () => {
		const reload = vi.fn();
		// The browser runs 10 minutes behind the server.
		vi.setSystemTime((NOW - 600) * 1000);
		render(PrivacyBanner, {
			overview: { privacy: { ...off, identifying_until: NOW + 3600 }, updated_at: NOW },
			reload
		});
		expect(screen.getByTestId('privacy-identifying').textContent).toContain('about 1 h more');
		await vi.advanceTimersByTimeAsync(50 * 60_000);
		await waitFor(() =>
			expect(screen.getByTestId('privacy-identifying').textContent).toContain('about 10 min more')
		);
		expect(reload).not.toHaveBeenCalled();
		await vi.advanceTimersByTimeAsync(10 * 60_000 + 15_000);
		await waitFor(() => expect(screen.queryByTestId('privacy-identifying')).toBeNull());
		expect(reload).toHaveBeenCalledTimes(1);
	});

	it('shows until turned off without a countdown', () => {
		render(PrivacyBanner, {
			overview: { privacy: { ...off, identifying_until: -1 }, updated_at: NOW },
			reload: () => {}
		});
		expect(screen.getByTestId('privacy-identifying').textContent).toContain(
			'until you turn it off'
		);
	});

	it('Turn off now sends only the off change, then reads the summary again', async () => {
		vi.useRealTimers();
		let on = true;
		get.mockImplementation((path: string) => {
			if (path === '/logs/catalog')
				return Promise.resolve({ now: NOW, kinds: kinds(on ? -1 : 0), events: [], presets: {} });
			if (path === '/logs/files') return Promise.resolve({ files: [] });
			return Promise.resolve({
				revision: 7,
				fields: {
					data: { value: 'hashes' },
					data_until: { value: -1 },
					kinds: { value: { size: 0 } },
					console_detail: { value: 'private' }
				}
			});
		});
		patch.mockImplementation(() => {
			on = false;
			return Promise.resolve({ sections: { logging: { revision: 8 } } });
		});
		const reload = vi.fn();
		render(PrivacyBanner, {
			overview: { privacy: { ...off, identifying_until: -1 }, updated_at: NOW },
			reload
		});
		const button = screen.getByRole('button', { name: 'Turn off now' });
		button.focus();
		expect(document.activeElement).toBe(button);
		await fireEvent.click(button);
		await waitFor(() => expect(reload).toHaveBeenCalledTimes(1), { timeout: 10_000 });
		expect(patch).toHaveBeenCalledTimes(1);
		const body = patch.mock.calls[0]![1].body.logging;
		// Back to Private with every deadline 0; the non-identifying switch is kept as it was.
		expect(body).toEqual({
			if_revision: 7,
			data: 'private',
			data_until: 0,
			kinds: { size: 0 },
			peer_activity_until: 0,
			change_details_until: 0
		});
	});
});
