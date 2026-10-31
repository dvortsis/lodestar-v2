// Settings → Trackers, Integrations, Notifications and Insights & digest (S-settings-2b) against
// the OpenAPI mock, by keyboard alone, with axe: each screen renders, a test shows its result, a
// change saves, and the risky actions ask first.
import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../src/lib/api/schema';

const expect = baseExpect.configure({ timeout: 10_000 });
type Index = components['schemas']['SettingsIndex'];
type Values = components['schemas']['SectionValues'];

const SECTIONS: Record<string, Record<string, Record<string, unknown>>> = {
	trackers: {
		scrape: { type: 'boolean', 'x-group': 'scrape', 'x-order': 0 },
		trackers: { type: 'array', items: { type: 'string' }, 'x-group': 'scrape', 'x-order': 1 }
	},
	integrations: {
		clients: { type: 'array', 'x-group': 'download_clients', 'x-order': 0 },
		default_client: { type: 'string', 'x-group': 'download_clients', 'x-order': 1 },
		torznab_url: { type: 'string', format: 'url', 'x-group': 'torznab', 'x-order': 2 },
		torznab_api_key: { type: 'string', writeOnly: true, 'x-group': 'torznab', 'x-order': 3 }
	},
	notifications: {
		discord_url: { type: 'string', writeOnly: true, 'x-group': 'discord', 'x-order': 4 },
		route_alerts: { type: 'array', 'x-group': 'routing', 'x-order': 15 }
	},
	insights: {
		digest_channels: { type: 'array', 'x-group': 'digest', 'x-order': 3 },
		query_logging: { type: 'boolean', 'x-danger': 'privacy', 'x-group': 'usage', 'x-order': 4 }
	}
};
const index = {
	sections: Object.entries(SECTIONS).map(([section, properties]) => ({
		section,
		label_key: `settings.${section}.label`,
		help_key: `settings.${section}.help`,
		fields: Object.keys(properties).map((name) => ({
			name,
			env: null,
			label_key: `settings.${section}.${name}.label`,
			help_key: `settings.${section}.${name}.help`,
			read_only: false,
			restart_required: section === 'trackers'
		})),
		schema: { type: 'object', properties }
	}))
} as unknown as Index;

const v = (value: unknown, more: Record<string, unknown> = {}) => ({
	value,
	env: null,
	read_only: false,
	restart_required: false,
	source: 'default' as const,
	...more
});
const HOME = {
	id: 'home',
	name: 'Home',
	kind: 'qbittorrent',
	url: 'http://192.168.1.5:8080/',
	username: 'admin',
	category: '',
	save_path: '',
	paused: false
};
const values: Values[] = [
	{
		section: 'trackers',
		revision: 1,
		restart_required: [],
		fields: { scrape: v(true, { restart_required: true }), trackers: v(['udp://t.example:1337']) }
	},
	{
		section: 'integrations',
		revision: 2,
		restart_required: [],
		fields: {
			clients: v([HOME], { source: 'db', secrets_set: ['home.password'] }),
			default_client: v(''),
			torznab_url: v(''),
			torznab_api_key: v(null)
		}
	},
	{
		section: 'notifications',
		revision: 1,
		restart_required: [],
		fields: { discord_url: v(null), route_alerts: v([]) }
	},
	{
		section: 'insights',
		revision: 1,
		restart_required: [],
		fields: { digest_channels: v([]), query_logging: v(false) }
	}
];

test.describe('keyboard: connection settings screens', () => {
	let backend: SettingsBackend;
	let tests: unknown[];
	let cleared: unknown[];
	test.beforeEach(async ({ mock }) => {
		backend = new SettingsBackend(index, values).attach(mock as unknown as MockLike);
		tests = [];
		cleared = [];
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('testSettings', (r: { postDataJSON(): unknown }) => {
			tests.push(r.postDataJSON());
			return {
				status: 200,
				body: { ok: true, error: null, version: 'v4.6.3', api_version: null }
			};
		});
		on('usage', () => ({ status: 200, body: { queries: 1204 } }));
		on('clearUsage', (r: { postDataJSON(): unknown }) => {
			cleared.push(r.postDataJSON());
			return { status: 200, body: { queries: 1204 } };
		});
	});

	test('Trackers saves a change by keyboard', async ({ page, go, kbd }) => {
		await go('/settings/trackers');
		await expect(page.getByRole('heading', { level: 1, name: 'Trackers' })).toBeVisible();
		await expectNoA11yViolations(page);
		await kbd.tabTo(page.getByRole('switch', { name: 'Ask trackers for peer counts' }));
		await kbd.press('Space');
		await kbd.press('Control+s');
		await expect(page.getByTestId('save-bar')).toContainText('Saved');
		expect(backend.patches).toEqual([{ trackers: { if_revision: 1, scrape: false } }]);
	});

	test('Integrations tests a client and saves an edit', async ({ page, go, kbd }) => {
		await go('/settings/integrations');
		await expect(page.getByRole('heading', { level: 1, name: 'Integrations' })).toBeVisible();
		const card = page.getByRole('group', { name: 'Home' });
		await expect(card).toBeVisible();
		await expectNoA11yViolations(page);
		await kbd.tabTo(card.getByLabel('Category'));
		await page.keyboard.type('lodestar');
		await kbd.tabTo(card.getByRole('button', { name: 'Test connection to Home' }));
		await kbd.press('Enter');
		await expect(card.getByTestId('test-result')).toHaveText('Connected. Version v4.6.3.');
		expect(tests).toEqual([
			{
				kind: 'download_client',
				client: 'qbittorrent',
				url: 'http://192.168.1.5:8080/',
				username: 'admin',
				id: 'home'
			}
		]);
		await kbd.press('Control+s');
		await expect(page.getByTestId('save-bar')).toContainText('Saved');
		expect(backend.patches).toEqual([
			{ integrations: { if_revision: 2, clients: [{ ...HOME, category: 'lodestar' }] } }
		]);
	});

	test('Notifications sends a test with the typed address', async ({ page, go, kbd }) => {
		await go('/settings/notifications');
		await expect(page.getByRole('heading', { level: 1, name: 'Notifications' })).toBeVisible();
		await expectNoA11yViolations(page);
		const discord = page.getByRole('region', { name: 'Discord' });
		await kbd.tabTo(discord.getByRole('button', { name: 'Add' }));
		await kbd.press('Enter');
		await kbd.tabTo(discord.getByLabel('New value for Discord webhook address'));
		await page.keyboard.type('https://discord.com/api/webhooks/1/x');
		await kbd.tabTo(discord.getByRole('button', { name: 'Send test to Discord' }));
		await kbd.press('Enter');
		await expect(discord.getByTestId('test-result')).toHaveText('Test message sent.');
		expect(tests).toEqual([{ kind: 'discord', url: 'https://discord.com/api/webhooks/1/x' }]);
		expect(backend.patches).toHaveLength(0);
	});

	test('Insights asks before recording searches, and before clearing usage data', async ({
		page,
		go,
		kbd
	}) => {
		await go('/settings/insights');
		await expect(page.getByRole('heading', { level: 1, name: 'Insights & digest' })).toBeVisible();
		await expectNoA11yViolations(page);
		await kbd.tabTo(page.getByRole('switch', { name: 'Record searches' }));
		await kbd.press('Space');
		await kbd.press('Control+s');
		const privacy = page.getByRole('dialog', { name: 'Record searches?' });
		await expect(privacy.getByRole('button', { name: 'Cancel' })).toBeFocused();
		await kbd.press('Tab');
		await expect(privacy.getByRole('button', { name: 'Record searches' })).toBeFocused();
		await kbd.press('Enter');
		await expect(page.getByTestId('save-bar')).toContainText('Saved');
		expect(backend.patches).toEqual([{ insights: { if_revision: 1, query_logging: true } }]);

		await kbd.tabTo(page.getByRole('button', { name: 'Clear usage data…' }));
		await kbd.press('Enter');
		const clear = page.getByRole('dialog', { name: 'Clear usage data?' });
		await expect(clear).toContainText('This deletes 1,204 logged searches.');
		await expect(clear.getByRole('button', { name: 'Cancel' })).toBeFocused();
		await kbd.press('Escape');
		await expect(clear).toBeHidden();
		expect(cleared).toHaveLength(0);
		await kbd.press('Enter'); // focus returned to the button
		await kbd.press('Tab');
		await kbd.press('Enter');
		await expect(page.getByText('Deleted 1,204 searches.')).toBeVisible();
		expect(cleared).toEqual([{ confirm: true }]);
	});
});
