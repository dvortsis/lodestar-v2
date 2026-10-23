import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { clearIndexCache } from '../form/schema';
import { dismissAll } from '../form/dialogs';
import { toasts } from '$lib/ui/feedback/toasts.svelte';
import { flow, stopWaiting } from './flow.svelte';
import { checkBody, settingsApi, systemInfo, type Replies } from './testing';
import ScreenHarness from './testing/ScreenHarness.svelte';
import type { SystemInfo } from './system';

const route = vi.hoisted(() => ({
	page: {
		params: {} as Record<string, string | undefined>,
		url: new URL('http://x/settings'),
		state: {}
	}
}));
vi.mock('$app/state', () => ({ page: route.page }));
vi.mock('$app/navigation', () => ({
	beforeNavigate: () => {},
	afterNavigate: () => {},
	goto: vi.fn(),
	replaceState: vi.fn()
}));

beforeAll(async () => {
	await Promise.all([import('../form/ConfirmDialog.svelte'), import('./Banners.svelte')]);
});

let system: SystemInfo;
let extra: Replies;
const reply =
	(body: unknown, status = 200) =>
	() => ({ status, body });
function api(over: Replies = {}) {
	return settingsApi({
		'GET /system': () => ({ status: 200, body: system }),
		'GET /network/check': reply(checkBody()),
		...extra,
		...over
	});
}
function open(path = '/settings', section?: string) {
	route.page.params = section ? { section } : {};
	route.page.url = new URL(`http://x${path}`);
}

beforeEach(() => {
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	vi.stubGlobal('matchMedia', () => ({
		matches: true,
		addEventListener: () => {},
		removeEventListener: () => {}
	}));
	clearIndexCache();
	system = systemInfo();
	extra = {};
	toasts.items = [];
	stopWaiting();
	open();
});
afterEach(() => {
	dismissAll();
	vi.useRealTimers();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

const text = (id: string) => screen.getByTestId(id).textContent?.replace(/\s+/g, ' ').trim();

describe('the Overview', () => {
	it('says nothing needs attention and that nothing is changed', async () => {
		api();
		render(ScreenHarness, {});
		expect(await screen.findByTestId('settings-attention-none')).toBeTruthy();
		expect(text('settings-attention-none')).toBe('Nothing needs attention.');
		expect(text('settings-changed-none')).toBe('Nothing is changed from its default.');
		expect(screen.getByRole('heading', { level: 2, name: 'Needs attention' })).toBeTruthy();
		expect(screen.getByRole('heading', { level: 2, name: 'Changed from default' })).toBeTruthy();
	});

	it('lists each item with one action, problems first, and puts the problem on its badge', async () => {
		system = systemInfo({ restart_required: ['crawler.encryption'] });
		api({
			'GET /network/check': reply(
				checkBody({ reachable: false, reason: 'no_unsolicited_queries' })
			),
			'GET /settings/network': reply({
				section: 'network',
				revision: 1,
				restart_required: [],
				fields: {
					dht_port: {
						value: 6881,
						source: 'default',
						env: null,
						read_only: false,
						restart_required: true
					}
				}
			})
		});
		render(ScreenHarness, {});
		const list = await screen.findByTestId('settings-attention');
		const rows = within(list).getAllByRole('listitem');
		expect(rows.map((r) => r.getAttribute('data-testid'))).toEqual([
			'settings-attention-dht-unreachable',
			'settings-attention-restart'
		]);
		expect(rows[0]!.textContent).toContain("Port 6881 isn't reachable from the internet");
		expect(
			within(rows[0]!).getByRole('link', { name: 'Open Network & VPN' }).getAttribute('href')
		).toBe('/settings/network');
		expect(within(rows[1]!).getByRole('button', { name: 'Restart now' })).toBeTruthy();
		// Badges come from the same items and are part of the link's name.
		const nav = screen.getByRole('navigation', { name: 'Settings sections' });
		expect(within(nav).getByRole('link', { name: 'Network & VPN, 1 problem' })).toBeTruthy();
		expect(within(nav).getByRole('link', { name: 'Crawler, Restart needed' })).toBeTruthy();
		expect(text('settings-badge-network')).toBe('1 problem');
	});

	it('counts the changed settings per screen, from the server’s list', async () => {
		system = systemInfo({
			diagnostics: {
				logging: null,
				recent_errors: null,
				changed: [
					{ name: 'crawler.intensity', source: 'db' },
					{ name: 'crawler.timeout_secs', source: 'file' },
					{ name: 'transport.utp', source: 'db' },
					{ name: 'network.dht_port', source: 'env' },
					{ name: 'logging.verbosity', source: 'db' }
				]
			}
		});
		api();
		render(ScreenHarness, {});
		await screen.findByTestId('settings-changed-crawler');
		expect(text('settings-changed-crawler')).toBe('3 changed');
		expect(text('settings-changed-network')).toBe('1 changed');
		expect(text('settings-changed-logging')).toBe('1 changed');
		expect(screen.queryByTestId('settings-changed-storage')).toBeNull();
		expect(screen.getByTestId('settings-changed-crawler').closest('a')!.getAttribute('href')).toBe(
			'/settings/crawler?changed=1'
		);
	});

	it('shows the other rows and says once that the network check could not be read', async () => {
		system = systemInfo({ restart_required: ['crawler.encryption'] });
		api({ 'GET /network/check': reply({ error: { code: 'x', message: 'x' } }, 500) });
		render(ScreenHarness, {});
		await screen.findByTestId('settings-attention-restart');
		expect(screen.getAllByTestId('settings-attention-partial')).toHaveLength(1);
		expect(text('settings-attention-partial')).toContain("Reachability couldn't be checked");
	});

	it('says so with Retry when /system fails, and the navigation still works', async () => {
		let fail = true;
		api({
			'GET /system': () =>
				fail
					? { status: 500, body: { error: { code: 'x', message: 'x' } } }
					: { status: 200, body: system }
		});
		render(ScreenHarness, {});
		expect((await screen.findByText(/Couldn't load the system status/)).textContent).toBeTruthy();
		const nav = screen.getByRole('navigation', { name: 'Settings sections' });
		expect(within(nav).getByRole('link', { name: 'Crawler' })).toBeTruthy();
		fail = false;
		await fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
		expect(await screen.findByTestId('settings-attention-none')).toBeTruthy();
	});

	it('shows a skeleton only when the wait is noticeable', async () => {
		vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
		let release!: () => void;
		const gate = new Promise<void>((r) => (release = r));
		const server = api();
		const realFetch = globalThis.fetch;
		vi.stubGlobal('fetch', async (url: string, init?: RequestInit) => {
			if (String(url).endsWith('/system')) await gate;
			return realFetch(url, init);
		});
		void server;
		render(ScreenHarness, {});
		const overview = await screen.findByTestId('settings-overview');
		expect(overview.getAttribute('aria-busy')).toBe('true');
		expect(screen.queryByTestId('skeleton')).toBeNull();
		await vi.advanceTimersByTimeAsync(299);
		expect(screen.queryByTestId('skeleton')).toBeNull();
		await vi.advanceTimersByTimeAsync(2);
		expect(screen.getByTestId('skeleton')).toBeTruthy();
		release();
		await vi.waitFor(() => expect(screen.queryByTestId('settings-attention-none')).not.toBeNull());
		expect(screen.queryByTestId('skeleton')).toBeNull();
	});
});

describe('the restart banner', () => {
	const bannerText = () => text('settings-restart-count');

	async function onCrawler() {
		open('/settings/crawler', 'crawler');
		render(ScreenHarness, { id: 'crawler' });
		return screen.findByTestId('settings-restart-banner');
	}

	it('counts the changes on every Settings page and offers Restart now', async () => {
		system = systemInfo({
			restart_required: ['crawler.encryption', 'network.dht_port', 'storage.mmap_mib']
		});
		api();
		await onCrawler();
		expect(bannerText()).toBe('3 changes apply after a restart');
		expect(screen.getByTestId('settings-restart-now')).toBeTruthy();
		expect(screen.queryByTestId('settings-restart-how')).toBeNull();
	});

	it('uses the singular for one change', async () => {
		system = systemInfo({ restart_required: ['crawler.encryption'] });
		api();
		await onCrawler();
		expect(bannerText()).toBe('1 change applies after a restart');
	});

	it.each([
		['docker', 'Restart the container to apply them.'],
		['unraid', 'Restart the container to apply them.'],
		['windows_service', 'Restart the Lodestar service in Windows Services to apply them.'],
		['windows_portable', 'Close Lodestar and start it again to apply them.']
	] as const)('tells a %s install how to restart, instead of a button', async (kind, words) => {
		system = systemInfo({
			restart_required: ['crawler.encryption'],
			deployment: { image_digest: null, image_tag: null, kind },
			restart: { available: false, estimated_seconds: 20, hint: 'ignored', method: 'manual' }
		});
		api();
		await onCrawler();
		expect(text('settings-restart-how')).toBe(words);
		expect(screen.queryByTestId('settings-restart-now')).toBeNull();
	});

	it('falls back to the server’s hint for any other install', async () => {
		system = systemInfo({
			restart_required: ['crawler.encryption'],
			deployment: { image_digest: null, image_tag: null, kind: 'systemd' },
			restart: {
				available: false,
				estimated_seconds: 20,
				hint: 'Run systemctl restart lodestar.',
				method: 'manual'
			}
		});
		api();
		await onCrawler();
		expect(text('settings-restart-how')).toBe('Run systemctl restart lodestar.');
	});

	it('is not there when nothing waits', async () => {
		api();
		open('/settings/crawler', 'crawler');
		render(ScreenHarness, { id: 'crawler' });
		await screen.findByRole('heading', { level: 1, name: 'Crawler' });
		await waitFor(() => expect(screen.queryByTestId('settings-restart-banner')).toBeNull());
	});

	it('reviews each field with its screen and saved value', async () => {
		system = systemInfo({ restart_required: ['crawler.encryption', 'network.dht_port'] });
		api({
			'GET /settings/crawler': reply({
				section: 'crawler',
				revision: 2,
				restart_required: ['encryption'],
				fields: {
					encryption: {
						value: 'require',
						source: 'db',
						env: null,
						read_only: false,
						restart_required: true
					}
				}
			}),
			'GET /settings/network': reply({
				section: 'network',
				revision: 2,
				restart_required: ['dht_port'],
				fields: {
					dht_port: {
						value: 7000,
						source: 'db',
						env: null,
						read_only: false,
						restart_required: true
					}
				}
			})
		});
		await onCrawler();
		await fireEvent.click(screen.getByRole('button', { name: 'Review' }));
		const enc = await screen.findByTestId('settings-review-crawler.encryption');
		expect(enc.textContent?.replace(/\s+/g, ' ')).toContain('Connection encryption');
		expect(enc.textContent).toContain('Saved: Require');
		expect(within(enc).getByRole('link', { name: 'Crawler' }).getAttribute('href')).toBe(
			'/settings/crawler#crawler.encryption'
		);
		const port = screen.getByTestId('settings-review-network.dht_port');
		expect(port.textContent).toContain('Saved: 7,000');
		expect(within(port).getByRole('link', { name: 'Network & VPN' })).toBeTruthy();
		expect(
			screen.getByText('Lodestar keeps running with the previous values until it restarts.')
		).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: 'Hide' }));
		expect(screen.queryByTestId('settings-restart-review')).toBeNull();
	});

	it('asks first, then restarts and shows the full-page Restarting state', async () => {
		system = systemInfo({
			restart_required: ['crawler.encryption'],
			restart: { available: true, estimated_seconds: 20, hint: null, method: 'reexec' }
		});
		const server = api({
			'POST /system/restart': reply({ estimated_seconds: 25, restarting: true }, 202)
		});
		await onCrawler();
		await fireEvent.click(screen.getByTestId('settings-restart-now'));
		const dialog = await screen.findByRole(
			'dialog',
			{ name: 'Restart Lodestar?' },
			{ timeout: 10_000 }
		);
		expect(dialog.textContent).toContain('Crawling pauses for about 20 sec');
		await waitFor(() =>
			expect(document.activeElement).toBe(within(dialog).getByRole('button', { name: 'Cancel' }))
		);
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(server.calls.filter((c) => c.method === 'POST')).toHaveLength(0);

		await fireEvent.click(screen.getByTestId('settings-restart-now'));
		await fireEvent.click(
			await screen.findByRole('button', { name: 'Restart' }, { timeout: 10_000 })
		);
		const restarting = await screen.findByTestId('settings-restarting', undefined, {
			timeout: 10_000
		});
		expect(restarting.textContent).toContain('Restarting…');
		expect(restarting.textContent).toContain('usually within 25 sec');
		expect(flow.phase).toBe('waiting');
		expect(flow.before).toBe(1_000);
	});

	it('shows the server’s message when it cannot restart (409 restart_unavailable)', async () => {
		system = systemInfo({ restart_required: ['crawler.encryption'] });
		api({
			'POST /system/restart': reply(
				{ error: { code: 'restart_unavailable', message: 'Restart it with docker restart.' } },
				409
			)
		});
		await onCrawler();
		await fireEvent.click(screen.getByTestId('settings-restart-now'));
		await fireEvent.click(await screen.findByRole('button', { name: 'Restart' }));
		await waitFor(() => expect(toasts.items).toHaveLength(1));
		expect(toasts.items[0]!.text).toBe(
			"Lodestar can't restart itself here. Restart it with docker restart."
		);
		expect(flow.phase).toBe('idle');
	});
});

describe('the address banner', () => {
	const change = (over = {}) => ({
		deadline: Math.floor(Date.now() / 1000) + 300,
		kind: 'http_bind' as const,
		previous_url: 'http://192.168.1.5:8090/',
		state: 'awaiting_confirmation' as const,
		stored: true,
		url: 'http://192.168.1.5:9090/',
		...over
	});
	async function show() {
		open('/settings/network', 'network');
		render(ScreenHarness, { id: 'network' });
		return screen.findByTestId('settings-address-banner');
	}

	it('asks to keep the address, with a countdown that ticks and does not announce itself', async () => {
		vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
		system = systemInfo({ address_change: change() });
		api();
		await vi.waitFor(async () => expect(await show()).toBeTruthy());
		const banner = screen.getByTestId('settings-address-banner');
		expect(banner.textContent).toContain('Keep this address?');
		const visible = screen.getByTestId('settings-address-text');
		expect(visible.getAttribute('aria-hidden')).toBe('true');
		expect(visible.textContent?.replace(/\s+/g, ' ').trim()).toBe(
			'Lodestar goes back to http://192.168.1.5:8090/ in 5 min if nobody confirms.'
		);
		await vi.advanceTimersByTimeAsync(61_000);
		expect(visible.textContent).toContain('in 4 min');
		expect(
			within(banner)
				.getByText(/within a few minutes/)
				.classList.contains('sr-only')
		).toBe(true);
		expect(screen.queryByTestId('settings-address-unstored')).toBeNull();
	});

	it('says the protection is temporary when the change was not stored', async () => {
		system = systemInfo({ address_change: change({ stored: false }) });
		api();
		await show();
		expect(text('settings-address-unstored')).toBe(
			'This protection lasts only until Lodestar stops.'
		);
	});

	it('keeps the address with one POST and refreshes /system', async () => {
		system = systemInfo({ address_change: change() });
		const server = api({
			'POST /system/address/keep': () => {
				system = systemInfo({ address_change: null });
				return { status: 200, body: { address_change: null, restarting: false } };
			}
		});
		await show();
		await fireEvent.click(screen.getByTestId('settings-address-keep'));
		await waitFor(() => expect(screen.queryByTestId('settings-address-banner')).toBeNull());
		expect(server.calls.filter((c) => c.path === '/system/address/keep')).toHaveLength(1);
	});

	it('goes back, then waits for the previous address', async () => {
		system = systemInfo({ address_change: change() });
		api({ 'POST /system/address/revert': reply({ address_change: null, restarting: true }) });
		await show();
		await fireEvent.click(screen.getByTestId('settings-address-back'));
		await waitFor(() => expect(flow.phase).toBe('waiting'));
		expect(flow.to).toBe('http://192.168.1.5:8090/');
	});

	it('refreshes when the change is already gone (409 no_address_change)', async () => {
		system = systemInfo({ address_change: change() });
		api({
			'POST /system/address/keep': () => {
				system = systemInfo({ address_change: null });
				return { status: 409, body: { error: { code: 'no_address_change', message: 'none' } } };
			}
		});
		await show();
		await fireEvent.click(screen.getByTestId('settings-address-keep'));
		await waitFor(() => expect(screen.queryByTestId('settings-address-banner')).toBeNull());
		expect(toasts.items).toHaveLength(0);
	});

	it('shows a toast for any other failure and stays', async () => {
		system = systemInfo({ address_change: change() });
		api({ 'POST /system/address/keep': reply({ error: { code: 'internal', message: 'x' } }, 500) });
		await show();
		await fireEvent.click(screen.getByTestId('settings-address-keep'));
		await waitFor(() => expect(toasts.items).toHaveLength(1));
		expect(screen.getByTestId('settings-address-banner')).toBeTruthy();
	});
});
