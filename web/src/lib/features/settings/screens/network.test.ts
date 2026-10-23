import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import type { components } from '$lib/api/schema';
import { dismissAll } from '../form/dialogs';
import type { GateContext } from '../form/types';
import { loadSettingMessages } from '../form/messages';
import { lockoutGate, lockoutText } from './lockout';
import ReachabilityPanel, { REFRESH_MS } from './ReachabilityPanel.svelte';
import { settingsApi } from '../shell/testing';

type Check = components['schemas']['NetworkCheck'];

const check = (over: Partial<Check> = {}): Check => ({
	checked_at: 1,
	external: { agreeing: 14, ip: '203.0.113.9', port: 6881, voters: 16 },
	inbound_queries: 412,
	mapping: 'unavailable',
	measured_s: 590,
	min_window_s: 120,
	reachable: true,
	reason: 'unsolicited_queries',
	unsolicited_queries: 37,
	unsolicited_sources: 21,
	window_s: 600,
	window_started_at: 1,
	...over
});

const text = (id: string) => screen.getByTestId(id).textContent?.trim();

// The lazily loaded dialog, transformed once up front rather than inside a test's timeout.
beforeAll(async () => {
	await import('../form/ConfirmDialog.svelte');
});

beforeEach(() => {
	vi.spyOn(console, 'warn').mockImplementation(() => {});
});
afterEach(() => {
	vi.useRealTimers();
	dismissAll();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('the reachability panel', () => {
	it('says reachable, with the address and the evidence as counts', async () => {
		settingsApi({ 'GET /network/check': () => ({ status: 200, body: check() }) });
		render(ReachabilityPanel);
		await screen.findByText('Reachable from the internet');
		expect(text('settings-reach-address')).toBe('203.0.113.9:6881');
		expect(text('settings-reach-inbound')).toBe('412');
		expect(text('settings-reach-unsolicited')).toBe('37');
		expect(text('settings-reach-sources')).toBe('21');
		expect(text('settings-reach-reason')).toContain('Other nodes are reaching the DHT port');
	});

	it('brackets an IPv6 address', async () => {
		const external = { agreeing: 9, ip: '2001:db8::1', port: 6881, voters: 9 };
		settingsApi({ 'GET /network/check': () => ({ status: 200, body: check({ external }) }) });
		render(ReachabilityPanel);
		await screen.findByText('Reachable from the internet');
		expect(text('settings-reach-address')).toBe('[2001:db8::1]:6881');
	});

	it('shows unknown values as a dash with the name “Not known yet”', async () => {
		settingsApi({
			'GET /network/check': () => ({
				status: 200,
				body: check({
					reachable: null,
					reason: 'bootstrapping',
					external: { agreeing: 0, ip: null, port: null, voters: 0 },
					inbound_queries: null,
					unsolicited_queries: null,
					unsolicited_sources: null
				})
			})
		});
		render(ReachabilityPanel);
		await screen.findByTestId('settings-reach-address');
		for (const id of [
			'settings-reach-address',
			'settings-reach-inbound',
			'settings-reach-unsolicited',
			'settings-reach-sources'
		]) {
			const value = screen.getByTestId(id).querySelector('[aria-label]')!;
			expect(value.textContent).toBe('—');
			expect(value.getAttribute('aria-label')).toBe('Not known yet');
		}
	});

	it.each([
		['dht_off', false, 'The DHT is off'],
		['bootstrapping', null, 'still joining the DHT'],
		['measuring', null, 'Measuring. About 2 min left.'],
		['unsolicited_queries', true, 'Other nodes are reaching'],
		['few_unsolicited_sources', null, 'Only a few networks'],
		['no_unsolicited_queries', false, 'Forward the UDP port']
	] as const)('explains %s in words', async (reason, reachable, words) => {
		settingsApi({
			'GET /network/check': () => ({
				status: 200,
				body: check({ reason, reachable, measured_s: 0, min_window_s: 120 })
			})
		});
		render(ReachabilityPanel);
		await waitFor(() => expect(text('settings-reach-reason')).toContain(words));
		const status = text('settings-reach-status');
		expect(status).toBe(
			reachable === true
				? 'Reachable from the internet'
				: reachable === false
					? 'Not reachable'
					: 'Checking…'
		);
	});

	it('checks again, and says to wait on a 429, with the time when it is given', async () => {
		let rate = false;
		const api = settingsApi({
			'GET /network/check': () => ({ status: 200, body: check() }),
			'POST /network/check': () =>
				rate
					? {
							status: 429,
							body: { error: { code: 'rate_limited', message: 'slow down' } },
							headers: { 'Retry-After': '25' }
						}
					: { status: 200, body: check({ reachable: false, reason: 'no_unsolicited_queries' }) }
		});
		render(ReachabilityPanel);
		await screen.findByText('Reachable from the internet');
		await fireEvent.click(screen.getByTestId('settings-reach-again'));
		await screen.findByText('Not reachable');
		expect(api.calls.filter((c) => c.method === 'POST')).toHaveLength(1);
		rate = true;
		await fireEvent.click(screen.getByTestId('settings-reach-again'));
		await waitFor(() =>
			expect(text('settings-reach-wait')).toBe('Wait 25 sec before checking again.')
		);
	});

	it('asks again every 30 seconds while the page is visible, and not while hidden', async () => {
		vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
		const api = settingsApi({ 'GET /network/check': () => ({ status: 200, body: check() }) });
		const gets = () => api.calls.filter((c) => c.path === '/network/check').length;
		const view = render(ReachabilityPanel);
		await vi.waitFor(() => expect(gets()).toBe(1));
		await vi.advanceTimersByTimeAsync(REFRESH_MS);
		await vi.waitFor(() => expect(gets()).toBe(2));
		vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
		await vi.advanceTimersByTimeAsync(REFRESH_MS * 2);
		expect(gets()).toBe(2);
		view.unmount();
		vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible');
		await vi.advanceTimersByTimeAsync(REFRESH_MS * 2);
		expect(gets()).toBe(2);
	});

	it('says so when the check fails', async () => {
		settingsApi({
			'GET /network/check': () => ({ status: 500, body: { error: { code: 'x', message: 'x' } } })
		});
		render(ReachabilityPanel);
		expect((await screen.findByTestId('settings-reach-failed')).textContent).toContain(
			'Couldn’t check reachability'.replace('’', "'")
		);
	});
});

const preview = (fields: Record<string, { estimates?: unknown[]; warnings?: unknown[] }>) =>
	({
		sections: {
			network: {
				revision: 1,
				fields: Object.fromEntries(
					Object.entries(fields).map(([k, v]) => [
						k,
						{ estimates: [], warnings: [], restart_required: true, value: null, ...v }
					])
				)
			}
		}
	}) as unknown as GateContext['preview'];

describe('the lockout dialog', () => {
	beforeEach(() => loadSettingMessages([]));

	it('names the new address and the five-minute confirmation for http_bind', () => {
		const t = lockoutText({
			fields: ['network.http_bind'],
			preview: preview({
				http_bind: { estimates: [{ name: 'url', value: 'http://192.168.1.5:9090' }] }
			})
		});
		expect(t.title).toBe('Change the web address?');
		expect(t.body[0]).toBe('After a restart, Lodestar is at http://192.168.1.5:9090.');
		expect(t.body[1]).toContain('confirm within 5 minutes');
		expect(t.body[1]).toContain('goes back to the current address by itself');
	});

	it('still warns without a url estimate', () => {
		const t = lockoutText({ fields: ['network.http_bind'], preview: preview({ http_bind: {} }) });
		expect(t.body[0]).toBe('After a restart, Lodestar listens on the new address.');
	});

	it('names a DHT change’s effect from the preview’s warnings', () => {
		const t = lockoutText({
			fields: ['network.dht_port'],
			preview: preview({
				dht_port: { warnings: [{ code: 'port_moves', message: 'The DHT moves to port 7000.' }] }
			})
		});
		expect(t.title).toBe('Apply this DHT change?');
		expect(t.body).toEqual(['The DHT moves to port 7000.']);
	});

	it('falls back to its own words for dht_port and dht_enabled alone', () => {
		const port = lockoutText({ fields: ['network.dht_port'], preview: preview({ dht_port: {} }) });
		expect(port.body[0]).toContain('Forward it on your router');
		const off = lockoutText({
			fields: ['network.dht_enabled'],
			preview: preview({ dht_enabled: {} })
		});
		expect(off.body[0]).toContain('stops finding new torrents');
	});

	it('asks with Cancel focused and resolves true only for Save', async () => {
		const gate = lockoutGate();
		const ctx = { fields: ['network.dht_port' as const], preview: preview({ dht_port: {} }) };
		const asked = gate(ctx);
		const dialog = await screen.findByRole('dialog');
		const cancel = await screen.findByRole('button', { name: 'Cancel' });
		await waitFor(() => expect(document.activeElement).toBe(cancel));
		expect(dialog).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));
		await expect(asked).resolves.toBe(true);

		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		const second = gate(ctx);
		await fireEvent.click(await screen.findByRole('button', { name: 'Cancel' }));
		await expect(second).resolves.toBe(false);
	});
});
