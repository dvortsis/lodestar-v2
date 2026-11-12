import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { waitFor } from '@testing-library/svelte';
import { flushSync, tick } from 'svelte';
import { restart } from './restart.svelte';
import { watchRestart } from './watch.svelte';

const page = vi.hoisted(() => ({ url: new URL('http://x/settings') }));
vi.mock('$app/state', () => ({ page }));
const api = vi.hoisted(() => ({ get: vi.fn(), post: vi.fn() }));
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api
}));

// Waits for conditions with a deadline of 15 s; the test's own timeout must not cut them short.
vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 15_000 });
const announce = () => document.querySelector('[data-testid="restart-announce"]');
const banner = () => document.querySelector('[data-testid="restart-banner"]');

beforeEach(() => {
	restart.begin(null);
	page.url = new URL('http://x/settings');
});
afterEach(() => vi.restoreAllMocks());

const system = (restart_required: string[], address_change: unknown = null) => ({
	address_change,
	restart: { available: true, estimated_seconds: 4, hint: null, method: 'reexec' },
	restart_required,
	started_at: 1
});

it('has its live region in the page before anything is announced, then fills it', async () => {
	api.get.mockResolvedValue(system(['network.dht_port']));
	const set = vi.fn();
	const stop = watchRestart(set);
	expect(announce()).not.toBeNull();
	expect(announce()!.textContent).toBe('');
	await until(() => expect(announce()!.textContent).toBe('1 change applies after a restart'));
	expect(announce()!.getAttribute('role')).toBe('status');
	stop();
	expect(announce()).toBeNull();
});

it('tells the pill, and floats the banner on the body once a change waits', async () => {
	api.get.mockResolvedValue(system(['network.dht_port', 'crawler.timeout_secs']));
	const set = vi.fn();
	const stop = watchRestart(set);
	await until(() => expect(set).toHaveBeenCalledWith(true));
	await until(() => expect(banner()).not.toBeNull());
	expect(banner()!.closest('[data-testid="restart-tray"]')!.parentElement).toBe(document.body);
	stop();
	await until(() => expect(banner()).toBeNull());
});

it('shows nothing while no change waits', async () => {
	api.get.mockResolvedValue(system([]));
	const set = vi.fn();
	const stop = watchRestart(set);
	await until(() => expect(set).toHaveBeenCalledWith(false));
	expect(banner()).toBeNull();
	expect(announce()!.textContent).toBe('');
	stop();
});

it('shows "Keep this address?" when nothing waits for a restart any more but the address awaits confirmation', async () => {
	// After the restart onto the new address the server reports nothing pending: the running and the
	// saved address match, and the change itself is what awaits.
	api.get.mockResolvedValue(
		system([], {
			deadline: Math.floor(Date.now() / 1000) + 300,
			kind: 'http_bind',
			previous_url: 'http://a:1/',
			state: 'awaiting_confirmation',
			stored: true,
			url: 'http://a:2/'
		})
	);
	page.url = new URL('http://x/search');
	const stop = watchRestart(vi.fn());
	await until(() => expect(document.body.textContent).toContain('Keep this address?'));
	expect(banner()).toBeNull();
	stop();
});

it('survives a partial /system', async () => {
	api.get.mockResolvedValue({ started_at: 1 });
	const set = vi.fn();
	const stop = watchRestart(set);
	await until(() => expect(set).toHaveBeenCalledWith(false));
	stop();
});

it('does not rewrite the live region when a refresh finds the same thing', async () => {
	// A new object with the same content on every call, as the server sends it.
	api.get.mockImplementation(async () => system(['network.dht_port']));
	const stop = watchRestart(vi.fn());
	await until(() => expect(announce()!.textContent).toBe('1 change applies after a restart'));
	const seen = vi.fn();
	new MutationObserver(seen).observe(announce()!, {
		childList: true,
		characterData: true,
		subtree: true
	});
	for (let i = 0; i < 3; i++) {
		await restart.refresh();
		await tick();
		flushSync();
	}
	// Observers deliver after the microtask checkpoint.
	await Promise.resolve();
	expect(api.get.mock.calls.length).toBeGreaterThan(3);
	expect(seen.mock.calls).toHaveLength(0);
	stop();
});

it('announces an address waiting to be kept once, in the same region', async () => {
	api.get.mockResolvedValue(
		system([], {
			deadline: Math.floor(Date.now() / 1000) + 300,
			kind: 'http_bind',
			previous_url: 'http://a:1/',
			state: 'awaiting_confirmation',
			stored: true,
			url: 'http://a:2/'
		})
	);
	const stop = watchRestart(vi.fn());
	await until(() => expect(announce()!.textContent).toMatch(/Lodestar is now at http:\/\/a:2\//));
	stop();
});
