// Settings → Logging (S-logs-2) against the OpenAPI mock, by keyboard alone, with axe: the screen
// renders, the Verbosity preset and an event change save together, turning on torrent names asks
// first (Cancel sends nothing), and the banner's Turn off now sends the change.
import { expectNoA11yViolations } from './a11y';
import { liveLogs } from './logs.stream';
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../src/lib/api/schema';

const expect = baseExpect.configure({ timeout: 10_000 });
type Index = components['schemas']['SettingsIndex'];
type Values = components['schemas']['SectionValues'];
type Catalog = components['schemas']['LogCatalog'];

const PROPS: Record<string, Record<string, unknown>> = {
	verbosity: {
		type: 'string',
		enum: ['errors', 'warnings', 'normal', 'detailed', 'everything'],
		'x-widget': 'radio',
		'x-group': 'events',
		'x-order': 0
	},
	events: { type: 'object', 'x-widget': 'log-events', 'x-group': 'events', 'x-order': 1 },
	components: {
		type: 'object',
		'x-widget': 'log-components',
		'x-advanced': true,
		'x-group': 'events',
		'x-order': 4
	},
	libraries: {
		type: 'string',
		format: 'log-filter',
		'x-advanced': true,
		'x-group': 'events',
		'x-order': 5
	},
	data: { type: 'string', enum: ['private', 'hashes', 'full'], 'x-group': 'data', 'x-order': 6 },
	console_format: {
		type: 'string',
		enum: ['text', 'json'],
		'x-widget': 'radio',
		'x-group': 'console',
		'x-order': 11
	},
	console_detail: {
		type: 'string',
		enum: ['private', 'file'],
		'x-widget': 'radio',
		'x-danger': 'privacy',
		'x-preview': true,
		'x-group': 'console',
		'x-order': 12
	},
	file_enabled: { type: 'boolean', 'x-group': 'file', 'x-order': 14, 'x-preview': true },
	file_size_mib: {
		type: 'integer',
		minimum: 1,
		maximum: 1024,
		'x-unit': 'mebibytes',
		'x-advanced': true,
		'x-preview': true,
		'x-group': 'file',
		'x-order': 17
	}
};
const index = {
	sections: [
		{
			section: 'logging',
			label_key: 'settings.logging.label',
			help_key: 'settings.logging.help',
			fields: Object.keys(PROPS).map((name) => ({
				name,
				env: null,
				label_key: `settings.logging.${name}.label`,
				help_key: `settings.logging.${name}.help`,
				read_only: false,
				restart_required: false
			})),
			schema: { type: 'object', properties: PROPS }
		}
	]
} as unknown as Index;
const v = (value: unknown) => ({
	value,
	env: null,
	read_only: false,
	restart_required: false,
	source: 'default' as const
});
const values: Values[] = [
	{
		section: 'logging',
		revision: 1,
		restart_required: [],
		fields: {
			verbosity: v('normal'),
			events: v({}),
			components: v({}),
			libraries: v('warn'),
			data: v('private'),
			data_until: v(0),
			kinds: v({}),
			console_detail: v('private'),
			console_format: v('text'),
			file_enabled: v(true),
			file_size_mib: v(16)
		}
	}
];

const NOW = Math.floor(Date.now() / 1000);
const KINDS = ['hash', 'name', 'path', 'url', 'query', 'client', 'peer', 'size'] as const;
const catalog = (nameUntil = 0): Catalog => ({
	now: NOW,
	events: [
		{
			id: 'pipeline.stored',
			component: 'pipeline',
			level: 'info',
			default_on: true,
			current: true,
			required: false,
			opt_in: null,
			kinds: ['hash', 'name'],
			label_key: 'logs.event.pipeline.stored.label',
			help_key: 'logs.event.pipeline.stored.help',
			rate_per_hour: 1200
		},
		{
			id: 'app.start',
			component: 'system',
			level: 'info',
			default_on: true,
			current: true,
			required: true,
			opt_in: null,
			kinds: [],
			label_key: 'logs.event.app.start.label',
			help_key: 'logs.event.app.start.help',
			rate_per_hour: 0
		}
	],
	kinds: [
		...KINDS.map((kind) => ({
			kind,
			identifying: kind !== 'client' && kind !== 'size',
			opt_in: false,
			default_on: kind === 'client' || kind === 'size',
			until: kind === 'client' || kind === 'size' ? -1 : kind === 'name' ? nameUntil : 0,
			outputs: { console: false, file: true, live: true, event_log: false }
		})),
		...(['peer_activity', 'filter_changes'] as const).map((kind) => ({
			kind,
			identifying: false,
			opt_in: true,
			default_on: false,
			until: 0,
			outputs: { console: false, file: false, live: false, event_log: false }
		}))
	],
	outputs: [],
	presets: { data: nameUntil ? 'custom' : 'private', verbosity: 'normal', temporary: null }
});

test.describe('keyboard: Settings → Logging', () => {
	let backend: SettingsBackend;
	let current: Catalog;
	let catalogFails = false;
	let deletes: unknown[] = [];
	test.beforeEach(async ({ mock, page }) => {
		// `E2E_CPU_THROTTLE=4` slows the CPU (Chromium) to check these flows on a slow runner.
		const rate = Number(process.env.E2E_CPU_THROTTLE ?? 0);
		if (rate > 1) {
			const cdp = await page.context().newCDPSession(page);
			await cdp.send('Emulation.setCPUThrottlingRate', { rate });
		}
		backend = new SettingsBackend(index, values).attach(mock as unknown as MockLike);
		current = catalog();
		catalogFails = false;
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('getLogCatalog', () =>
			catalogFails
				? { status: 500, body: { error: { code: 'internal', message: 'down' } } }
				: { status: 200, body: current }
		);
		deletes = [];
		on('getLogFiles', () => ({
			status: 200,
			body: {
				files: [
					{
						name: 'lodestar.jsonl',
						size: 4096,
						details: true,
						opt_in: false,
						current: true,
						compressed: false,
						from: '2026-10-31T10:00:00Z',
						to: '2026-10-31T11:00:00Z'
					}
				],
				folder: '/data/logs',
				index_readable: true,
				reason: null,
				state: 'on'
			}
		}));
		on('deleteLogFiles', (r: { postDataJSON(): unknown }) => {
			deletes.push(r.postDataJSON());
			return { status: 200, body: { deleted: 1, bytes: 4096, skipped: [] } };
		});
	});

	test('renders, and saves Verbosity with an event change', async ({ page, go, kbd }) => {
		await go('/settings/logging');
		await expect(page.getByRole('heading', { level: 1, name: 'Logging' })).toBeVisible();
		await expect(page.getByTestId('log-kind-name')).toBeVisible();
		await expectNoA11yViolations(page);
		await kbd.tabTo(page.getByRole('radio', { name: 'Normal (info)' }), 40);
		await kbd.press('ArrowRight');
		await expect(page.getByRole('radio', { name: 'Detailed (debug)' })).toBeFocused();
		await kbd.tabTo(page.getByTestId('log-group-pipeline'), 40);
		await kbd.press('Enter');
		await kbd.tabTo(page.getByRole('switch', { name: 'pipeline.stored' }), 20);
		await kbd.press('Space');
		// A required event can't be turned off, and says why.
		await kbd.tabTo(page.getByTestId('log-group-system'), 20);
		await kbd.press('Enter');
		await expect(page.getByRole('switch', { name: 'Lodestar started' })).toBeDisabled();
		await expect(page.getByTestId('log-event-app.start')).toContainText('Required');
		await kbd.press('Control+s');
		await expect(page.getByTestId('save-bar')).toContainText('Saved');
		expect(backend.patches).toEqual([
			{
				logging: {
					if_revision: 1,
					verbosity: 'detailed',
					events: { 'pipeline.stored': false }
				}
			}
		]);
	});

	test('asks before torrent names turn on; Cancel sends nothing', async ({ page, go, kbd }) => {
		await go('/settings/logging');
		const names = page.getByRole('switch', { name: 'Torrent names' });
		await kbd.tabTo(names, 80);
		await kbd.press('Space');
		const dialog = page.getByRole('dialog', { name: 'Write torrent details to the log?' });
		await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused();
		await expectNoA11yViolations(page);
		await kbd.press('Enter');
		await expect(dialog).toBeHidden();
		await expect(names).toBeFocused();
		await expect(names).not.toBeChecked();
		expect(backend.patches).toEqual([]);
	});

	test('the banner shows while names are on, and Turn off now sends Private', async ({
		page,
		go,
		kbd
	}) => {
		current = catalog(NOW + 1800);
		await go('/settings/logging');
		await expect(page.getByTestId('logging-banner-identifying')).toContainText('30 min');
		await expectNoA11yViolations(page);
		await kbd.tabTo(page.getByRole('button', { name: 'Turn off now' }), 80);
		current = catalog();
		await kbd.press('Enter');
		await expect(page.getByTestId('logging-banner-identifying')).toBeHidden();
		expect(backend.patches).toEqual([
			{ logging: { if_revision: 1, data: 'private', data_until: 0, kinds: {} } }
		]);
	});

	test('Reset to preset puts changed events back; Show changed only lists them', async ({
		page,
		go,
		kbd
	}) => {
		await go('/settings/logging');
		await kbd.tabTo(page.getByTestId('log-group-pipeline'), 60);
		await kbd.press('Enter');
		const stored = page.getByRole('switch', { name: 'pipeline.stored' });
		await expect(stored).toBeVisible();
		await kbd.tabTo(stored, 20);
		await kbd.press('Space');
		await expect(stored).not.toBeChecked();
		// The list's toolbar sits above the rows: back to it with Shift+Tab.
		const changedOnly = page.getByRole('checkbox', { name: 'Show changed only' });
		for (
			let i = 0;
			i < 20 && !(await changedOnly.evaluate((el) => el === document.activeElement));
			i++
		)
			await kbd.press('Shift+Tab');
		await expect(changedOnly).toBeFocused();
		await kbd.press('Space');
		await expect(page.getByTestId('log-event-app.start')).toBeHidden();
		await expect(page.getByTestId('log-event-pipeline.stored')).toBeVisible();
		await kbd.tabTo(page.getByRole('button', { name: 'Reset to preset' }), 10);
		await kbd.press('Enter');
		await expect(page.getByText('No events match.')).toBeVisible();
		await kbd.press('Shift+Tab');
		await expect(changedOnly).toBeFocused();
		await kbd.press('Space');
		await expect(page.getByRole('switch', { name: 'pipeline.stored' })).toBeChecked();
	});

	test('/logs: Turn off now sends Private and both opt-ins off', async ({
		page,
		go,
		kbd,
		mock
	}) => {
		await liveLogs(page);
		mock.on('getLogHistory', { lines: [], more: false, next_cursor: null, unreadable: 0 });
		current = catalog(NOW + 1800);
		await go('/logs');
		const banner = page.getByText('Log lines may identify torrents or people');
		await expect(banner).toBeVisible();
		await expectNoA11yViolations(page);
		await kbd.tabTo(page.getByRole('button', { name: 'Turn off now' }), 40);
		current = catalog();
		await kbd.press('Enter');
		await expect(banner).toBeHidden();
		expect(backend.patches).toEqual([
			{
				logging: {
					if_revision: 1,
					data: 'private',
					data_until: 0,
					kinds: {},
					peer_activity_until: 0,
					change_details_until: 0
				}
			}
		]);
	});

	test("/logs: when Turn off now fails and the state can't be read, the banner stays", async ({
		page,
		go,
		kbd,
		mock
	}) => {
		await liveLogs(page);
		mock.on('getLogHistory', { lines: [], more: false, next_cursor: null, unreadable: 0 });
		current = catalog(NOW + 1800);
		await go('/logs');
		const banner = page.getByText('Log lines may identify torrents or people');
		await expect(banner).toBeVisible();
		backend.nextPatch.push({
			status: 500,
			body: { error: { code: 'internal', message: 'disk full' } }
		});
		catalogFails = true;
		await kbd.tabTo(page.getByRole('button', { name: 'Turn off now' }), 40);
		await kbd.press('Enter');
		await expect(page.getByRole('alert')).toContainText("Couldn't turn it off");
		await expect(banner).toBeVisible();
	});

	test("outputs: the console's detail asks first; files download redacted; delete asks", async ({
		page,
		go,
		kbd
	}) => {
		await go('/settings/logging');
		const files = page.getByTestId('log-file-lodestar.jsonl');
		await expect(files).toContainText('Contains torrent details');
		await expect(page.getByRole('link', { name: 'Download lodestar.jsonl' })).toHaveAttribute(
			'href',
			'/api/v1/logs/files/lodestar.jsonl?format=text&redact=true'
		);
		await expectNoA11yViolations(page);
		// Remove torrent details is on by default; unticking it by keyboard changes the links.
		const redact = page.getByRole('checkbox', { name: 'Remove torrent details' });
		await kbd.tabTo(redact, 200);
		await expect(redact).toBeChecked();
		await kbd.press('Space');
		await kbd.tabTo(page.getByTestId('log-download-all'), 5);
		await expect(page.getByTestId('log-download-all')).toHaveAttribute(
			'href',
			'/api/v1/logs/archive.zip?format=text&redact=false'
		);
		await kbd.press('Shift+Tab');
		await kbd.press('Space');
		await expect(redact).toBeChecked();

		await kbd.tabTo(page.getByRole('radio', { name: 'Private', exact: true }).last(), 120);
		await kbd.press('ArrowRight');
		await kbd.press('Control+s');
		const ask = page.getByRole('dialog', { name: 'Show torrent details in the console?' });
		await expect(ask.getByRole('button', { name: 'Cancel' })).toBeFocused();
		await kbd.press('Enter');
		await expect(ask).toBeHidden();
		expect(backend.patches).toEqual([]);
		await kbd.press('Control+s');
		await expect(ask.getByRole('button', { name: 'Cancel' })).toBeFocused();
		await kbd.press('Tab');
		await kbd.press('Enter');
		await expect(page.getByTestId('save-bar')).toContainText('Saved');
		expect(backend.patches).toEqual([{ logging: { if_revision: 1, console_detail: 'file' } }]);

		await kbd.tabTo(page.getByRole('button', { name: 'Delete files with torrent details…' }), 80);
		await kbd.press('Enter');
		const del = page.getByRole('dialog', { name: 'Delete the log files with torrent details?' });
		await expect(del).toContainText('Log files to delete: 1');
		await expect(del.getByRole('button', { name: 'Cancel' })).toBeFocused();
		await kbd.press('Tab');
		await kbd.press('Enter');
		await expect(page.getByText(/Log files deleted: 1/)).toBeVisible();
		expect(deletes).toEqual([{ which: 'details' }]);
	});
});
