// Settings → About (S-about) against the OpenAPI mock, by keyboard alone, with axe: the screen
// renders the build and the connections, a tab left open across an upgrade is offered a reload,
// Check now runs the signed check, the diagnostics are shown before they are copied, and a server
// that doesn't answer still leaves the page standing.
import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../src/lib/api/schema';

const expect = baseExpect.configure({ timeout: 10_000 });
type Index = components['schemas']['SettingsIndex'];
type System = components['schemas']['SystemInfo'];
type Updates = components['schemas']['UpdatesStatus'];

const index = { sections: [] } as unknown as Index;
const NOW = Math.floor(Date.now() / 1000);

const system = (over: Partial<System> = {}): System =>
	({
		address_change: null,
		build: {
			commit: '0123456789abcdef0123456789abcdef01234567',
			date: 1_760_000_000,
			rustc: 'rustc 1.90.0',
			target: 'x86_64-unknown-linux-musl',
			ui_build_id: null
		},
		channel: 'stable',
		connections: [
			{ kind: 'dht', state: 'on', count: null, last_checked_at: null, settings: 'crawler' },
			{
				kind: 'blocklist_subscriptions',
				state: 'on',
				count: 2,
				last_checked_at: NOW - 3600,
				settings: 'filters'
			},
			{ kind: 'workers', state: null, count: null, last_checked_at: null, settings: 'workers' }
		],
		data: {
			cold_bytes: null,
			database_bytes: 5_000_000,
			dir: '/data',
			filesystem_warning: null,
			free_bytes: 90_000_000_000,
			index_bytes: 20_000_000,
			logs_bytes: null
		},
		data_formats: { index: 'v3-t2', rebuild: null, store_schema: 12 },
		deployment: { image_digest: null, image_tag: '0.5.2', kind: 'docker' },
		diagnostics: {
			changed: [{ name: 'network.port', source: 'env' }],
			logging: 'normal',
			recent_errors: 0
		},
		fixture: false,
		host: { arch: 'x86_64', cpus: 4, memory_bytes: 8_000_000_000, os: 'linux', time_zone: 'UTC' },
		repository: 'https://github.com/example/lodestar',
		restart: { can_restart: false, pending: false },
		restart_required: [],
		started_at: NOW - 7200,
		uptime_s: 7200,
		version: '0.5.2',
		...over
	}) as unknown as System;

const updates = (over: Partial<Updates> = {}): Updates =>
	({
		capability: { can_install: false, reason: null },
		history: [],
		notices: [],
		pending: null,
		policy: {},
		releases: [],
		rollback: {},
		running: {
			channel: 'stable',
			image_version: null,
			overlay: false,
			revoked_key: false,
			version: '0.5.2',
			withdrawn: false
		},
		status: { checked_at: NOW - 600, error: null, next_check_at: null, state: 'ok' },
		unavailable: null,
		...over
	}) as unknown as Updates;

test.describe('keyboard: Settings → About', () => {
	let checks: number;
	let current: System;
	let state: Updates;
	test.beforeEach(async ({ mock }) => {
		new SettingsBackend(index, []).attach(mock as unknown as MockLike);
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		checks = 0;
		current = system();
		state = updates();
		on('getSystem', () => ({ status: 200, body: current }));
		on('getUpdates', () => ({ status: 200, body: state }));
		on('checkUpdates', () => {
			checks++;
			state = updates({
				releases: [
					{
						version: '0.5.3',
						date: NOW,
						notes: 'Fixes.',
						notes_url: 'https://github.com/example/lodestar/releases/tag/v0.5.3',
						blocked_by: [],
						eligible: false,
						installs_at: null,
						level: 'fix',
						manual_steps: false,
						migrates: false,
						reindex: false,
						reindex_bytes: 0,
						security: false
					}
				]
			});
			return { status: 200, body: state };
		});
	});

	test('the logo stays visible when illustrations are off', async ({ page, go }) => {
		await go('/settings/about');
		await page.evaluate(() => (document.documentElement.dataset.illustrations = 'false'));
		await expect(page.getByRole('img', { name: 'Lodestar', exact: true })).toBeVisible();
	});

	test('renders the build and what it connects to, and passes axe', async ({ page, go }) => {
		await go('/settings/about');
		await expect(page.getByRole('heading', { level: 2, name: 'Lodestar 0.5.2' })).toBeVisible();
		await expect(page.getByText('x86_64-unknown-linux-musl')).toBeVisible();
		await expect(page.getByRole('link', { name: 'DHT and peers' })).toBeVisible();
		await expect(page.getByText('Not in this version')).toBeVisible();
		await expect(page.getByText('This page is from an older version')).toHaveCount(0);
		await expectNoA11yViolations(page);
	});

	test('offers a reload when the server runs another interface build', async ({ page, go }) => {
		current = system({ build: { ...system().build, ui_build_id: 'another-build' } });
		await go('/settings/about');
		await expect(page.getByText('This page is from an older version of Lodestar.')).toBeVisible();
		await expect(page.getByRole('button', { name: 'Reload' })).toBeVisible();
	});

	test('Check now runs the signed check and shows the release', async ({ page, go, kbd }) => {
		await go('/settings/about');
		await kbd.tabTo(page.getByRole('button', { name: 'Check now' }), 80);
		await kbd.press('Enter');
		await expect(page.getByText('Lodestar 0.5.3 is available.')).toBeVisible();
		expect(checks).toBe(1);
	});

	test('shows the diagnostics before anything is copied', async ({ page, go, kbd }) => {
		await go('/settings/about');
		await kbd.tabTo(page.getByRole('button', { name: 'Show diagnostics' }), 80);
		await kbd.press('Enter');
		const block = page.getByTestId('about-diagnostics');
		await expect(block).toContainText('network.port (env)');
		await expect(block).not.toContainText('/data');
		await kbd.tabTo(page.getByRole('button', { name: 'Report a problem' }), 5);
		await kbd.press('Enter');
		await expect(
			page.getByText('Nothing is sent unless you submit the issue on GitHub.')
		).toBeVisible();
		await expectNoA11yViolations(page);
	});

	test('the licence list opens on request', async ({ page, go, kbd }) => {
		await go('/settings/about');
		await kbd.tabTo(page.getByRole('button', { name: 'Show open-source components' }), 100);
		await kbd.press('Enter');
		await expect(page.getByRole('searchbox', { name: 'Search components' })).toBeVisible();
	});

	test('keeps the page when the server does not answer', async ({ page, go, mock }) => {
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('getSystem', () => ({
			status: 500,
			body: { error: { code: 'internal', message: 'down' } }
		}));
		await go('/settings/about');
		await expect(page.getByRole('button', { name: 'Try again' })).toBeVisible();
		await expect(page.getByText('Every node is a new discovery')).toBeVisible();
	});
});
