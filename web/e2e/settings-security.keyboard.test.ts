// Settings → Security (M-security-settings) against the OpenAPI mock, by keyboard alone, with axe:
// the screen renders; a password change; and Sign out other sessions behind the password
// confirmation, where Cancel sends nothing and a wrong password shows the server's words.
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
			'Every other session was signed out'
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
			'Every other session was signed out.'
		);
		expect(sent.map((s) => s.body)).toEqual([
			{ password: 'wrong-password' },
			{ password: PASSWORD }
		]);
	});
});
