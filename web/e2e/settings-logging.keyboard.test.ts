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
	data: { type: 'string', enum: ['private', 'hashes', 'full'], 'x-group': 'data', 'x-order': 6 }
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
			console_detail: v('private')
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
	test.beforeEach(async ({ mock }) => {
		backend = new SettingsBackend(index, values).attach(mock as unknown as MockLike);
		current = catalog();
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('getLogCatalog', () => ({ status: 200, body: current }));
		on('getLogFiles', () => ({
			status: 200,
			body: { files: [], folder: '/data/logs', index_readable: true, reason: null, state: 'on' }
		}));
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
		await expect(page.getByRole('switch', { name: 'app.start' })).toBeDisabled();
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
});
