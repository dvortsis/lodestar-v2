import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import RestartingPage from './RestartingPage.svelte';
import { restart } from './restart.svelte';

const api = vi.hoisted(() => ({ get: vi.fn() }));
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api
}));

const until = (check: () => void) => waitFor(check, { timeout: 15_000 });
const assign = vi.fn();
const reload = vi.fn();

beforeEach(() => {
	vi.useFakeTimers({ shouldAdvanceTime: true });
	api.get.mockReset();
	assign.mockReset();
	reload.mockReset();
	vi.stubGlobal('location', {
		hostname: 'nas.local',
		origin: 'http://nas.local:8090',
		pathname: '/settings',
		assign,
		reload
	});
});
afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
	restart.begin(null);
});

const fetchOk = () => vi.fn<typeof fetch>(async () => new Response('ok'));

it('is a modal dialog with a live status line, and everything behind it inert', () => {
	vi.stubGlobal(
		'fetch',
		vi.fn(() => new Promise(() => {}))
	);
	const behind = document.body.appendChild(document.createElement('div'));
	const { unmount } = render(RestartingPage, {
		props: { target: null, startedAt: 1, estimateSeconds: 4 }
	});
	expect(screen.getByRole('dialog', { name: 'Restarting…' })).toBeTruthy();
	expect(screen.getByRole('status').textContent).toMatch(/about 4 seconds/);
	expect(behind.hasAttribute('inert')).toBe(true);
	unmount();
	expect(behind.hasAttribute('inert')).toBe(false);
	behind.remove();
});

it('reloads once the new run answers and nothing is pending', async () => {
	vi.stubGlobal('fetch', fetchOk());
	api.get.mockResolvedValue({ started_at: 2, restart_required: [] });
	render(RestartingPage, { props: { target: null, startedAt: 1, estimateSeconds: 0 } });
	await vi.advanceTimersByTimeAsync(1_500);
	await until(() => expect(reload).toHaveBeenCalled());
});

it('says so when something still waits for a restart, and offers Reload', async () => {
	vi.stubGlobal('fetch', fetchOk());
	api.get.mockResolvedValue({ started_at: 2, restart_required: ['network.dht_port'] });
	render(RestartingPage, { props: { target: null, startedAt: 1, estimateSeconds: 0 } });
	await vi.advanceTimersByTimeAsync(1_500);
	await until(() => expect(screen.getByRole('button', { name: 'Reload' })).toBeTruthy());
	expect(reload).not.toHaveBeenCalled();
});

it('never fetches another origin: it waits for this address to go quiet, then goes once', async () => {
	const probe = vi.fn<typeof fetch>(async () => Promise.reject(new TypeError('down')));
	vi.stubGlobal('fetch', probe);
	render(RestartingPage, {
		props: { target: 'http://nas.local:9000/', startedAt: 1, estimateSeconds: 1 }
	});
	// The way there is always on the page.
	expect(screen.getByTestId('restarting-open').getAttribute('href')).toBe(
		'http://nas.local:9000/settings'
	);
	await vi.advanceTimersByTimeAsync(6_000);
	await until(() => expect(assign).toHaveBeenCalledTimes(1));
	expect(assign).toHaveBeenCalledWith('http://nas.local:9000/settings');
	for (const [url] of probe.mock.calls) expect(String(url)).toBe('/healthz');
});

it('stays on the page, with the link, while the old address still answers', async () => {
	const probe = fetchOk();
	vi.stubGlobal('fetch', probe);
	render(RestartingPage, {
		props: { target: 'http://nas.local:9000/', startedAt: 1, estimateSeconds: 1 }
	});
	await vi.advanceTimersByTimeAsync(10_000);
	expect(assign).not.toHaveBeenCalled();
	expect(screen.getByTestId('restarting-open')).toBeTruthy();
	expect(screen.getByRole('status').textContent).toMatch(/nas\.local:9000/);
	for (const [url] of probe.mock.calls) expect(String(url)).toBe('/healthz');
});

it('gives up after two minutes and says what to check', async () => {
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => Promise.reject(new TypeError('down')))
	);
	render(RestartingPage, { props: { target: null, startedAt: 1, estimateSeconds: 4 } });
	await vi.advanceTimersByTimeAsync(130_000);
	await until(() =>
		expect(screen.getByTestId('restarting-checks').textContent).toMatch(/healthcheck/)
	);
	expect(screen.getByRole('button', { name: 'Keep waiting' })).toBeTruthy();
});

it('is not torn down by a /system refresh that finds nothing pending', async () => {
	api.get.mockResolvedValue({ restart_required: [] });
	restart.begin({ target: null, startedAt: 1, estimateSeconds: 4 });
	await restart.refresh();
	expect(api.get).not.toHaveBeenCalled();
	expect(restart.waiting).not.toBeNull();
});
