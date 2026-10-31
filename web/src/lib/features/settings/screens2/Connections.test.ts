// Integrations, Notifications and Insights & digest (S-settings-2b) on the form engine, against an
// in-memory `/settings` API: tests with the values on screen, secrets, the Privacy dialog and
// Clear usage data.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import { dismissAll } from '../form/dialogs';
import { fakeServer } from '../form/testing/fake-server';
import { spec, sv, values } from '../form/testing/values';
import type { FieldSpec } from '../form/types';
import { screens } from './screen';

const index = vi.hoisted(() => ({ specs: [] as unknown[] }));
vi.mock('../form/schema', async (actual) => ({
	...(await actual<typeof import('../form/schema')>()),
	loadSettingsIndex: () =>
		Promise.resolve({
			fieldsOf: (sections: string[]) =>
				(index.specs as FieldSpec[]).filter((s) => sections.includes(s.section))
		})
}));
vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: vi.fn() }));

const until = (check: () => void) => waitFor(check, { timeout: 10_000 });
vi.setConfig({ testTimeout: 30_000 });

const SPECS: FieldSpec[] = [
	spec('integrations.clients', { kind: 'unknown', group: 'download_clients', order: 0 }),
	spec('integrations.default_client', { group: 'download_clients', order: 1 }),
	spec('integrations.torznab_url', { group: 'torznab', order: 2, format: 'url' }),
	spec('integrations.torznab_api_key', { group: 'torznab', order: 3, writeOnly: true }),
	spec('notifications.ntfy_url', { group: 'ntfy', order: 0, writeOnly: true }),
	spec('notifications.ntfy_priority', { kind: 'integer', group: 'ntfy', order: 2, advanced: true }),
	spec('notifications.discord_url', { group: 'discord', order: 4, writeOnly: true }),
	spec('notifications.webhook_url', { group: 'webhook', order: 13, writeOnly: true }),
	spec('notifications.email_host', { group: 'email', order: 5 }),
	spec('notifications.email_from', { group: 'email', order: 10 }),
	spec('notifications.email_to', { kind: 'string-list', group: 'email', order: 11 }),
	spec('notifications.route_alerts', { kind: 'unknown', group: 'routing', order: 15 }),
	spec('insights.digest', { kind: 'enum', enum: ['off', 'daily', 'weekly'], group: 'digest' }),
	spec('insights.digest_channels', { kind: 'unknown', group: 'digest', order: 3 }),
	spec('insights.query_logging', { kind: 'boolean', group: 'usage', order: 4, danger: 'privacy' }),
	spec('insights.query_retention_days', {
		kind: 'integer',
		minimum: 1,
		maximum: 30,
		group: 'usage',
		order: 5
	})
];

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
const OK = { status: 200, body: { ok: true, error: null, version: 'v4.6.3', api_version: null } };

let server: ReturnType<typeof fakeServer>;

beforeEach(() => {
	sessionStorage.clear();
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	index.specs = SPECS;
	server = fakeServer([
		values('integrations', 3, {
			clients: sv([HOME], 'db', { secrets_set: ['home.password'] }),
			default_client: sv(''),
			torznab_url: sv('http://prowlarr.lan/1/api', 'db'),
			torznab_api_key: sv(null)
		}),
		values('notifications', 1, {
			ntfy_url: sv(null),
			ntfy_priority: sv(0),
			discord_url: sv(null),
			webhook_url: sv(null),
			email_host: sv(''),
			email_from: sv(''),
			email_to: sv([]),
			route_alerts: sv([])
		}),
		values('insights', 1, {
			digest: sv('off'),
			digest_channels: sv([]),
			query_logging: sv(false),
			query_retention_days: sv(30)
		})
	]);
	vi.stubGlobal('fetch', server.fetch);
});
afterEach(() => {
	dismissAll();
	vi.unstubAllGlobals();
	initLocale('en');
});

async function open(id: string) {
	const Screen = (await screens.find((s) => s.id === id)!.load!()).default;
	render(Screen);
}
const type = (el: HTMLElement, value: string) => fireEvent.input(el, { target: { value } });
const posted = (path: string) => server.requests('POST', path).map((c) => c.body);
const patched = () => server.requests('PATCH', '/settings').map((c) => c.body);
/** The dialog `ask()` mounted on the page. */
const dialog = () => screen.findByRole('dialog', {}, { timeout: 10_000 });

describe('Integrations', () => {
	it('tests a client with the values on screen, and never sends its saved password', async () => {
		server.next('POST /settings/test', OK);
		await open('integrations');
		const entry = await screen.findByRole('group', { name: 'Home' });
		await type(within(entry).getByLabelText('Address'), 'http://10.0.0.2:8080/');
		await fireEvent.click(within(entry).getByRole('button', { name: 'Test connection to Home' }));
		await until(() =>
			expect(entry.querySelector('[data-testid="test-result"]')?.textContent?.trim()).toBe(
				'Connected. Version v4.6.3.'
			)
		);
		expect(posted('/settings/test')).toEqual([
			{
				kind: 'download_client',
				client: 'qbittorrent',
				url: 'http://10.0.0.2:8080/',
				username: 'admin',
				id: 'home'
			}
		]);
	});

	it('sends a password typed for the test, and says why a test failed as plain text', async () => {
		server.next('POST /settings/test', {
			status: 200,
			body: {
				ok: false,
				version: null,
				api_version: null,
				error: { code: 'bad_credentials', message: '<b>Refused</b>: wrong password' }
			}
		});
		await open('integrations');
		const entry = await screen.findByRole('group', { name: 'Home' });
		await type(within(entry).getByLabelText('New password'), 'hunter2');
		await fireEvent.click(within(entry).getByRole('button', { name: 'Test connection to Home' }));
		const result = () => entry.querySelector('[data-testid="test-result"]')!;
		await until(() => expect(result().textContent).toContain('wrong password'));
		// Text, not markup.
		expect(result().textContent).toBe('<b>Refused</b>: wrong password');
		expect(result().querySelector('b')).toBeNull();
		expect(posted('/settings/test')[0]).toMatchObject({ password: 'hunter2', id: 'home' });
	});

	it('shows a server error as text', async () => {
		server.next('POST /settings/test', server.err(502, 'bad_gateway'));
		await open('integrations');
		const entry = await screen.findByRole('group', { name: 'Home' });
		await fireEvent.click(within(entry).getByRole('button', { name: 'Test connection to Home' }));
		await until(() =>
			expect(entry.querySelector('[data-testid="test-result"]')?.textContent).toBe(
				'msg bad_gateway'
			)
		);
	});

	it('tests Torznab on the typed key, and without one uses the saved key', async () => {
		server.next('POST /settings/test', OK, OK);
		await open('integrations');
		const test = await screen.findByRole('button', { name: 'Test connection to Torznab' });
		await fireEvent.click(test);
		await until(() => expect(posted('/settings/test')).toHaveLength(1));
		expect(posted('/settings/test')[0]).toEqual({
			kind: 'torznab',
			url: 'http://prowlarr.lan/1/api'
		});
		const key = within(screen.getByRole('region', { name: 'Torznab' }));
		await fireEvent.click(await key.findByRole('button', { name: 'Add' }));
		await type(await key.findByLabelText('New value for Torznab API key'), 'k-123');
		await fireEvent.click(test);
		await until(() => expect(posted('/settings/test')).toHaveLength(2));
		expect(posted('/settings/test')[1]).toEqual({
			kind: 'torznab',
			url: 'http://prowlarr.lan/1/api',
			api_key: 'k-123'
		});
	});

	it('saves the list without the password of a client left alone', async () => {
		await open('integrations');
		const entry = await screen.findByRole('group', { name: 'Home' });
		await type(within(entry).getByLabelText('Category'), 'lodestar');
		await fireEvent.click(await screen.findByRole('button', { name: 'Save changes' }));
		await until(() => expect(patched()).toHaveLength(1));
		const sent = patched()[0] as { integrations: { clients: Record<string, unknown>[] } };
		expect(sent.integrations.clients).toEqual([{ ...HOME, category: 'lodestar' }]);
		expect(sent.integrations.clients[0]).not.toHaveProperty('password');
		// A password is never staged: no draft holds one either.
		expect(JSON.stringify({ ...sessionStorage })).not.toContain('password');
	});

	it('removes a password with its own confirmed request, apart from Save', async () => {
		await open('integrations');
		const entry = await screen.findByRole('group', { name: 'Home' });
		await type(within(entry).getByLabelText('Category'), 'staged');
		await until(() =>
			expect(within(entry).getByTestId('client-password-status').textContent).toContain('Set')
		);
		await fireEvent.click(within(entry).getByRole('button', { name: 'Remove password of Home' }));
		const confirm = await dialog();
		expect(patched()).toHaveLength(0);
		await fireEvent.click(within(confirm).getByRole('button', { name: 'Remove' }));
		await until(() => expect(patched()).toHaveLength(1));
		// The server's list as it is saved, not the staged edit; the password is "".
		expect(patched()[0]).toEqual({
			integrations: { if_revision: 3, clients: [{ ...HOME, password: '' }] }
		});
	});

	it('keeps the password when removing it is cancelled', async () => {
		await open('integrations');
		const entry = await screen.findByRole('group', { name: 'Home' });
		await fireEvent.click(
			await within(entry).findByRole('button', { name: 'Remove password of Home' })
		);
		const confirm = await dialog();
		await fireEvent.click(within(confirm).getByRole('button', { name: 'Cancel' }));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(patched()).toHaveLength(0);
		expect(within(entry).getByTestId('client-password-status').textContent).toContain('Set');
	});

	it('saves a new password on its own, for a saved client only', async () => {
		await open('integrations');
		const entry = await screen.findByRole('group', { name: 'Home' });
		await type(within(entry).getByLabelText('New password'), 's3cret');
		await fireEvent.click(within(entry).getByRole('button', { name: 'Save password for Home' }));
		await until(() => expect(patched()).toHaveLength(1));
		expect(patched()[0]).toEqual({
			integrations: { if_revision: 3, clients: [{ ...HOME, password: 's3cret' }] }
		});
		await until(() =>
			expect((within(entry).getByLabelText('New password') as HTMLInputElement).value).toBe('')
		);
	});

	it('keeps the password for the saved address: editing the address waits for Save', async () => {
		await open('integrations');
		const entry = await screen.findByRole('group', { name: 'Home' });
		await type(within(entry).getByLabelText('New password'), 'pw-for-b');
		const save = within(entry).getByRole('button', { name: 'Save password for Home' });
		await until(() => expect(save).toHaveProperty('disabled', false));
		await type(within(entry).getByLabelText('Address'), 'http://other.lan:8080/');
		await until(() => expect(save).toHaveProperty('disabled', true));
		expect(within(entry).getByText(/Save your changes first/)).toBeTruthy();
		expect(patched()).toHaveLength(0);
	});

	it('keeps focus on a move button of the client that moved', async () => {
		server.change('integrations', {
			clients: sv([HOME, { ...HOME, id: 'work', name: 'Work' }], 'db', {
				secrets_set: ['home.password']
			})
		});
		await open('integrations');
		const home = await screen.findByRole('group', { name: 'Home' });
		const down = within(home).getByRole('button', { name: 'Move Home down' });
		down.focus();
		await fireEvent.click(down);
		// Home is last now, so Down is disabled and focus goes to its Up.
		await until(() =>
			expect(document.activeElement).toBe(
				within(screen.getByRole('group', { name: 'Home' })).getByRole('button', {
					name: 'Move Home up'
				})
			)
		);
	});

	it('says the password state is unknown when it could not be checked', async () => {
		// The form's own load comes first; the cards' check for set passwords is the second request.
		server.next(
			'GET /settings/integrations',
			() => ({
				status: 200,
				body: structuredClone(server.sections.get('integrations'))
			}),
			server.err(500, 'internal')
		);
		await open('integrations');
		const entry = await screen.findByRole('group', { name: 'Home' });
		await until(() =>
			expect(within(entry).getByTestId('client-password-status').textContent).toContain(
				'Couldn’t check'
			)
		);
		expect(within(entry).queryByRole('button', { name: 'Remove password of Home' })).toBeNull();
	});

	it('does not reuse the id of a removed client that is still saved', async () => {
		server.change('integrations', {
			clients: sv([{ ...HOME, id: 'client-1' }], 'db', { secrets_set: ['client-1.password'] })
		});
		await open('integrations');
		const entry = await screen.findByRole('group', { name: 'Home' });
		await fireEvent.click(within(entry).getByRole('button', { name: 'Remove client Home' }));
		await fireEvent.click(await screen.findByRole('button', { name: 'Add a download client' }));
		const added = await screen.findByTestId('client-entry');
		expect(added.getAttribute('data-client')).not.toBe('client-1');
	});

	it('adds a client whose password waits for it to be saved', async () => {
		await open('integrations');
		await fireEvent.click(await screen.findByRole('button', { name: 'Add a download client' }));
		const entry = await screen.findByRole('group', { name: 'Client 2' });
		expect(
			within(entry).getByRole('button', { name: 'Save password for Client 2' })
		).toHaveProperty('disabled', true);
		expect(within(entry).getByText(/Save the client first/)).toBeTruthy();
	});
});

describe('Notifications', () => {
	it('sends a test with the values on screen, and says what failed', async () => {
		server.next(
			'POST /settings/test',
			{
				status: 200,
				body: {
					ok: false,
					version: null,
					api_version: null,
					error: { code: 'rejected', message: 'Discord refused it (404).' }
				}
			},
			OK
		);
		await open('notifications');
		const discord = within(await screen.findByRole('region', { name: 'Discord' }));
		await fireEvent.click(await discord.findByRole('button', { name: 'Add' }));
		await type(
			await discord.findByLabelText('New value for Discord webhook'),
			'https://discord.com/api/webhooks/1/x'
		);
		const send = await discord.findByRole('button', { name: 'Send test to Discord' });
		await fireEvent.click(send);
		await until(() => expect(discord.getByTestId('test-result').textContent).toContain('404'));
		await fireEvent.click(send);
		await until(() =>
			expect(discord.getByTestId('test-result').textContent).toBe('Test message sent.')
		);
		expect(posted('/settings/test')).toEqual([
			{ kind: 'discord', url: 'https://discord.com/api/webhooks/1/x' },
			{ kind: 'discord', url: 'https://discord.com/api/webhooks/1/x' }
		]);
		// Nothing was saved.
		expect(patched()).toHaveLength(0);
	});

	it('sends nothing for an email test that lacks a server, sender or recipient', async () => {
		await open('notifications');
		const email = within(await screen.findByRole('region', { name: 'Email' }));
		await fireEvent.click(await email.findByRole('button', { name: 'Send test to Email' }));
		await until(() =>
			expect(email.getByTestId('test-result').textContent).toContain('mail server')
		);
		expect(posted('/settings/test')).toHaveLength(0);
	});

	it('sends ntfy and webhook tests without the saved secrets, and with typed ones', async () => {
		server.next('POST /settings/test', OK, OK);
		await open('notifications');
		const ntfy = within(await screen.findByRole('region', { name: 'ntfy' }));
		await fireEvent.click(await ntfy.findByRole('button', { name: 'Send test to ntfy' }));
		await until(() => expect(posted('/settings/test')).toHaveLength(1));
		expect(posted('/settings/test')[0]).toEqual({
			kind: 'ntfy',
			priority: 0,
			allow_private: false
		});
		const webhook = within(await screen.findByRole('region', { name: 'Webhook' }));
		await fireEvent.click(await webhook.findByRole('button', { name: 'Add' }));
		await type(
			await webhook.findByLabelText('New value for Webhook address'),
			'https://hooks.example.com/x'
		);
		await fireEvent.click(webhook.getByRole('button', { name: 'Send test to Webhook' }));
		await until(() => expect(posted('/settings/test')).toHaveLength(2));
		expect(posted('/settings/test')[1]).toEqual({
			kind: 'webhook',
			url: 'https://hooks.example.com/x',
			allow_private: false
		});
	});

	it('routes events to channels with checkboxes', async () => {
		await open('notifications');
		const alerts = within(await screen.findByRole('group', { name: 'Alerts go to' }));
		await fireEvent.click(alerts.getByRole('checkbox', { name: 'Discord' }));
		await fireEvent.click(await screen.findByRole('button', { name: 'Save changes' }));
		await until(() => expect(patched()).toHaveLength(1));
		expect(patched()[0]).toMatchObject({ notifications: { route_alerts: ['discord'] } });
	});
});

describe('Insights & digest', () => {
	async function turnOnLogging() {
		await open('insights');
		const logging = await screen.findByRole('switch', { name: 'Record searches' });
		await fireEvent.click(logging);
		await fireEvent.click(await screen.findByRole('button', { name: 'Save changes' }));
		return dialog();
	}

	it('asks before it records searches, and Cancel saves nothing', async () => {
		const privacy = await turnOnLogging();
		expect(within(privacy).getByText(/text of each search/)).toBeTruthy();
		expect(within(privacy).getByText(/deleted after 30 days/)).toBeTruthy();
		await fireEvent.click(within(privacy).getByRole('button', { name: 'Cancel' }));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(patched()).toHaveLength(0);
	});

	it('saves once the Privacy dialog is confirmed', async () => {
		const privacy = await turnOnLogging();
		await fireEvent.click(within(privacy).getByRole('button', { name: 'Record searches' }));
		await until(() => expect(patched()).toHaveLength(1));
		expect(patched()[0]).toMatchObject({ insights: { query_logging: true } });
	});

	it('turns logging off without a dialog', async () => {
		server.change('insights', { query_logging: sv(true, 'db') });
		await open('insights');
		await fireEvent.click(await screen.findByRole('switch', { name: 'Record searches' }));
		await fireEvent.click(await screen.findByRole('button', { name: 'Save changes' }));
		await until(() => expect(patched()).toHaveLength(1));
		expect(screen.queryByRole('dialog')).toBeNull();
	});

	it('clears usage data only after a dialog with the exact count', async () => {
		server.next('GET /usage', { status: 200, body: { queries: 1204 } });
		server.next('POST /usage/clear', { status: 200, body: { queries: 1204 } });
		await open('insights');
		await fireEvent.click(await screen.findByRole('button', { name: 'Clear usage data…' }));
		const confirm = await dialog();
		expect(within(confirm).getByText('This deletes 1,204 logged searches.')).toBeTruthy();
		expect(posted('/usage/clear')).toHaveLength(0);
		const cancel = within(confirm).getByRole('button', { name: 'Cancel' });
		await until(() => expect(document.activeElement).toBe(cancel));
		await fireEvent.click(within(confirm).getByRole('button', { name: 'Delete 1,204 searches' }));
		await until(() => expect(posted('/usage/clear')).toEqual([{ confirm: true }]));
		await until(() => expect(screen.getByText('Deleted 1,204 searches.')).toBeTruthy());
	});

	it('deletes nothing when the dialog is cancelled, or when there is nothing to delete', async () => {
		server.next('GET /usage', { status: 200, body: { queries: 5 } });
		await open('insights');
		await fireEvent.click(await screen.findByRole('button', { name: 'Clear usage data…' }));
		await fireEvent.click(within(await dialog()).getByRole('button', { name: 'Cancel' }));
		server.next('GET /usage', { status: 200, body: { queries: 0 } });
		await fireEvent.click(screen.getByRole('button', { name: 'Clear usage data…' }));
		await until(() => expect(screen.getByText('There is no usage data to clear.')).toBeTruthy());
		expect(posted('/usage/clear')).toHaveLength(0);
	});

	it('shows a failed clear as text', async () => {
		server.next('GET /usage', { status: 200, body: { queries: 5 } });
		server.next('POST /usage/clear', server.err(428, 'confirmation_required'));
		await open('insights');
		await fireEvent.click(await screen.findByRole('button', { name: 'Clear usage data…' }));
		await fireEvent.click(
			within(await dialog()).getByRole('button', { name: 'Delete 5 searches' })
		);
		await until(() => expect(screen.getByText('msg confirmation_required')).toBeTruthy());
	});

	it('leaves a place for the digest preview', async () => {
		await open('insights');
		await screen.findByRole('switch', { name: 'Record searches' });
		expect(document.querySelector('[data-slot="digest-preview"]')).not.toBeNull();
	});
});
