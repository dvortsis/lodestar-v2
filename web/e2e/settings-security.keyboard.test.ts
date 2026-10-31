// Settings → Security (M-security-settings) against the OpenAPI mock, by keyboard alone, with axe:
// the screen renders; a password change; and Sign out other sessions behind the password
// confirmation, where Cancel sends nothing and a wrong password shows the server's words.
import { expectNoA11yViolations } from './a11y';
import { dialogSettled } from './support/dialog';
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../src/lib/api/schema';

const expect = baseExpect.configure({ timeout: 10_000 });
type Index = components['schemas']['SettingsIndex'];
type Values = components['schemas']['SectionValues'];

// The auth fixtures: only the network section's secure_cookies, read-only and set by the env.
const index = {
	sections: [
		{
			section: 'network',
			label_key: 'settings.network.label',
			help_key: 'settings.network.help',
			fields: [
				{
					name: 'secure_cookies',
					env: 'LODESTAR_SECURE_COOKIES',
					label_key: 'settings.network.secure_cookies.label',
					help_key: 'settings.network.secure_cookies.help',
					read_only: true,
					restart_required: true
				}
			],
			schema: {
				type: 'object',
				properties: { secure_cookies: { type: 'boolean', 'x-group': 'web_ui' } }
			}
		}
	]
} as unknown as Index;
const values: Values[] = [
	{
		section: 'network',
		revision: 1,
		restart_required: [],
		fields: {
			secure_cookies: {
				value: true,
				env: 'LODESTAR_SECURE_COOKIES',
				read_only: true,
				restart_required: true,
				source: 'env'
			}
		}
	}
];
const PASSWORD = 'current-password-1';

test.describe('keyboard: Settings → Security', () => {
	let sent: { op: string; body: unknown }[];
	test.beforeEach(async ({ mock }) => {
		new SettingsBackend(index, values).attach(mock as unknown as MockLike);
		sent = [];
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		const check = (op: string) => (r: { postDataJSON(): Record<string, unknown> }) => {
			const body = r.postDataJSON();
			sent.push({ op, body });
			const pw = op === 'changePassword' ? body.old : body.password;
			return pw === PASSWORD
				? { status: 200, body: { ok: true } }
				: {
						status: 401,
						body: {
							error: {
								code: 'bad_password',
								message: 'That password is wrong.',
								field: op === 'changePassword' ? 'old' : 'password'
							}
						}
					};
		};
		on('changePassword', check('changePassword'));
		on('revokeOtherSessions', check('revokeOtherSessions'));
		keysAndProxy(on, []);
	});

	test('renders with its status, and passes axe', async ({ page, go }) => {
		await go('/settings/security');
		await expect(page.getByRole('heading', { level: 1, name: 'Security' })).toBeVisible();
		await expect(page.getByTestId('security-cookies')).toHaveText('Secure cookies: On');
		await expect(page.getByText('Set by LODESTAR_SECURE_COOKIES.', { exact: false })).toBeVisible();
		await expectNoA11yViolations(page);
	});

	test('changes the password by keyboard', async ({ page, go, kbd }) => {
		await go('/settings/security');
		await kbd.tabTo(page.getByLabel('Current password'), 150);
		await page.keyboard.type(PASSWORD);
		await kbd.press('Tab');
		await page.keyboard.type('a-new-passphrase');
		await kbd.press('Tab');
		await page.keyboard.type('a-new-passphrase');
		await kbd.press('Enter');
		await expect(page.getByTestId('security-password-result')).toContainText(
			'Every other browser signed in with the password was signed out'
		);
		expect(sent).toEqual([
			{ op: 'changePassword', body: { old: PASSWORD, new: 'a-new-passphrase' } }
		]);
		await expect(page.getByLabel('Current password')).toHaveValue('');
	});

	test('signs out other sessions behind the password, by keyboard', async ({ page, go, kbd }) => {
		await go('/settings/security');
		const revoke = page.getByRole('button', { name: 'Sign out other sessions…' });
		await kbd.tabTo(revoke, 150);
		await kbd.press('Enter');
		let dialog = page.getByRole('dialog', { name: 'Sign out other sessions?' });
		await expect(dialog.getByLabel('Your password')).toBeFocused();
		await expectNoA11yViolations(page);
		await kbd.press('Escape');
		await expect(dialog).toBeHidden();
		expect(sent).toEqual([]);

		await kbd.tabTo(revoke, 150);
		await kbd.press('Enter');
		dialog = page.getByRole('dialog', { name: 'Sign out other sessions?' });
		await expect(dialog.getByLabel('Your password')).toBeFocused();
		await page.keyboard.type('wrong-password');
		await kbd.press('Enter');
		await expect(dialog.getByText('That password is wrong.')).toBeVisible();
		await expect(dialog.getByLabel('Your password')).toHaveValue('');
		await expect(dialog.getByLabel('Your password')).toBeFocused();
		await page.keyboard.type(PASSWORD);
		await kbd.press('Enter');
		await expect(dialog).toBeHidden();
		await expect(page.getByTestId('security-sessions-result')).toHaveText(
			'Every other browser signed in with the password was signed out.'
		);
		expect(sent.map((s) => s.body)).toEqual([
			{ password: 'wrong-password' },
			{ password: PASSWORD }
		]);
	});
});

// Part 2 (M-security-settings-2): a key created, revealed once and closed; turning proxy sign-in
// on behind its warning and the password. By keyboard alone, with axe on each dialog.
const SECRET = 'lsk_AbCdEfGh_0123456789abcdefghijklmnopqrstuvwxyzABCDEFG';
const key = (id: number, name: string) => ({
	id,
	name,
	kind: 'api_key',
	prefix: `lsk_k${id}abcdef`,
	scopes: ['torznab'],
	status: 'active',
	created_at: 1_790_000_000,
	expires_at: null,
	last_used_at: null,
	revoked_at: null,
	saved_search_id: null
});

/** The keys and proxy routes: the screen loads both, so every test here answers them. */
function keysAndProxy(
	on: (id: string, f: unknown) => unknown,
	sent: { op: string; body: unknown }[]
) {
	type Req = { postDataJSON(): Record<string, unknown> };
	on('listKeys', () => ({ status: 200, body: { keys: [key(1, 'Sonarr')] } }));
	on('createKey', (r: Req) => {
		const body = r.postDataJSON();
		sent.push({ op: 'createKey', body });
		return body.password === PASSWORD
			? { status: 201, body: { key: key(2, String(body.name)), secret: SECRET } }
			: {
					status: 401,
					body: { error: { code: 'bad_password', message: 'That password is wrong.' } }
				};
	});
	let proxy = {
		enabled: false,
		forced_off: false,
		header: 'Remote-User',
		trusted: [] as string[],
		allowed_users: [] as string[]
	};
	on('getProxyAuth', () => ({ status: 200, body: proxy }));
	on('putProxyAuth', (r: Req) => {
		const body = r.postDataJSON();
		sent.push({ op: 'putProxyAuth', body });
		const rest = { ...body };
		delete rest.password;
		proxy = { ...proxy, ...rest };
		return { status: 200, body: proxy };
	});
}

test.describe('keyboard: Settings → Security keys and proxy', () => {
	let sent: { op: string; body: unknown }[];
	test.beforeEach(async ({ mock }) => {
		new SettingsBackend(index, values).attach(mock as unknown as MockLike);
		sent = [];
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		keysAndProxy(on, sent);
	});

	test('creates a key, shows its secret once, and keeps it nowhere', async ({ page, go, kbd }) => {
		await go('/settings/security');
		await kbd.tabTo(page.getByRole('button', { name: 'Manage keys and tokens' }), 150);
		await kbd.press('Enter');
		await expect(page.getByText('Sonarr', { exact: true })).toBeVisible();
		await expect(page.getByRole('heading', { name: 'API keys and feed tokens' })).toBeFocused();
		await expectNoA11yViolations(page);
		await kbd.tabTo(page.getByRole('textbox', { name: 'Name' }), 200);
		await page.keyboard.type('Radarr');
		await kbd.tabTo(page.getByTestId('security-key-create'), 20);
		await kbd.press('Enter');
		const dialog = page.getByRole('dialog', { name: 'Create Radarr?' });
		await expect(dialog.getByLabel('Your password')).toBeFocused();
		await page.keyboard.type(PASSWORD);
		await kbd.press('Enter');
		const reveal = page.getByRole('dialog', { name: 'Copy the secret for Radarr' });
		await expect(page.getByRole('dialog')).toHaveCount(1);
		await dialogSettled(page, 'Copy the secret for Radarr');
		await expect(reveal.getByTestId('key-secret')).toHaveValue(SECRET);
		await expect(reveal.getByTestId('key-secret')).toBeFocused();
		await expectNoA11yViolations(page);
		expect(sent).toEqual([
			{
				op: 'createKey',
				body: { name: 'Radarr', kind: 'api_key', scopes: ['search'], password: PASSWORD }
			}
		]);
		await kbd.press('Escape');
		await expect(reveal).toBeHidden();
		await expect(page.getByText('Created Radarr.')).toBeVisible();
		await expect
			.poll(() =>
				page.evaluate(
					(s) =>
						[
							document.documentElement.outerHTML,
							JSON.stringify({ ...localStorage }),
							JSON.stringify({ ...sessionStorage }),
							location.href
						].some((t) => t.includes(s)),
					SECRET
				)
			)
			.toBe(false);
	});

	test('turns proxy sign-in on behind its warning and the password', async ({ page, go, kbd }) => {
		await go('/settings/security');
		await kbd.tabTo(page.getByRole('button', { name: 'Show proxy sign-in settings' }), 150);
		await kbd.press('Enter');
		const toggle = page.getByRole('switch', { name: "Sign people in from the proxy's header" });
		await kbd.tabTo(toggle, 250);
		await kbd.press('Space');
		await expect(page.getByTestId('security-proxy-warning')).toBeVisible();
		await kbd.press('Tab');
		await expect(page.getByRole('textbox', { name: 'Trusted proxies' })).toBeFocused();
		await page.keyboard.type('172.20.0.2');
		await kbd.tabTo(page.getByTestId('security-proxy-save'), 10);
		await kbd.press('Enter');
		let dialog = page.getByRole('dialog', { name: 'Turn on trusted proxy sign-in?' });
		await expect(dialog.getByLabel('Your password')).toBeFocused();
		await expect(dialog).toContainText('Anything at 172.20.0.2 will be able to sign in');
		await expectNoA11yViolations(page);
		await kbd.press('Escape');
		await expect(dialog).toBeHidden();
		expect(sent).toEqual([]);

		// Focus came back to Save.
		await expect(page.getByTestId('security-proxy-save')).toBeFocused();
		await kbd.press('Enter');
		dialog = page.getByRole('dialog', { name: 'Turn on trusted proxy sign-in?' });
		await expect(dialog.getByLabel('Your password')).toBeFocused();
		await page.keyboard.type(PASSWORD);
		await kbd.press('Enter');
		await expect(dialog).toBeHidden();
		await expect(page.getByTestId('security-proxy-result')).toHaveText('Saved.');
		await expect(page.getByTestId('security-proxy-warning')).toBeHidden();
		expect(sent).toEqual([
			{
				op: 'putProxyAuth',
				body: {
					enabled: true,
					trusted: ['172.20.0.2'],
					header: 'Remote-User',
					allowed_users: [],
					password: PASSWORD
				}
			}
		]);
	});
});
