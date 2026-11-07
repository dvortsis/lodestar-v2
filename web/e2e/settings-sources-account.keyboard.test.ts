// Settings → Sources (SRC-15c) by keyboard alone, with axe: the Account dialog (approved text,
// write-only credential fields, the server's refusal shown plainly while the gate is closed) and the
// purge dialog's counts before the password. Waits are conditions only. No secret is printed.
import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import { dht, source } from '../src/lib/features/sources/fixtures';
import type { components } from '../src/lib/api/schema';

type Source = components['schemas']['Source'];
const expect = baseExpect.configure({ timeout: 10_000 });

const SECRET = 'e2e-secret-value-1234';
const tracker = source({
	id: 5,
	name: 'Private Tracker',
	kinds: ['importer'],
	consent: 'account',
	needs: 'credentials',
	endpoints: ['https://tracker.example/api'],
	credential_spec: [
		{ name: 'username', kind: 'username', label: 'Username', required: true },
		{ name: 'api_key', kind: 'secret', label: 'API key', required: true }
	]
});

test.describe('keyboard: Sources account and purge', () => {
	let sources: Source[];
	let creds: unknown[];
	let enables: unknown[];
	test.beforeEach(async ({ mock }) => {
		new SettingsBackend().attach(mock as unknown as MockLike);
		sources = [dht, tracker];
		creds = [];
		enables = [];
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('listSources', () => ({ status: 200, body: { sources } }));
		on('patchSourceCredentials', (req: { postDataJSON(): { credentials: object } }) => {
			const body = req.postDataJSON();
			creds.push(body);
			sources = sources.map((s) =>
				s.id === 5 ? ({ ...s, credentials: Object.keys(body.credentials) } as Source) : s
			);
			return { status: 200, body: sources.find((s) => s.id === 5) };
		});
		on('patchSource', (req: { postDataJSON(): unknown }) => {
			enables.push(req.postDataJSON());
			return {
				status: 400,
				body: {
					error: {
						code: 'invalid_setting',
						message: 'this source asks for your consent before it is turned on, so it stays off.'
					}
				}
			};
		});
		on('previewPurge', () => ({
			status: 200,
			body: {
				claims_deleted: 96402,
				kept: 88120,
				plugin: 'x',
				queue_deleted: 0,
				settled: true,
				source_id: 5,
				state_deleted: 0,
				torrents_deleted: 1204331,
				unverified_deleted: 5210,
				unverified_kept: 0
			}
		}));
	});

	test('Account dialog: approved text, write-only fields, the refusal in plain words', async ({
		page,
		go,
		kbd
	}) => {
		await go('/settings/sources');
		const toggle = page.getByRole('switch', { name: 'Private Tracker: on' });
		await kbd.tabTo(toggle, 40);
		await kbd.press('Space');
		const dialog = page.getByRole('dialog', {
			name: 'Private Tracker signs in with your account.'
		});
		await expect(dialog).toBeVisible();
		await expect(dialog).toContainText(
			'Results found only through your account stay on this server'
		);
		const key = dialog.getByLabel('API key');
		await expect(key).toHaveAttribute('type', 'password');
		await expect(key).toHaveValue('');
		await expectNoA11yViolations(page);
		await dialog.getByLabel('Username').focus();
		await page.keyboard.type('me');
		await key.focus();
		await page.keyboard.type(SECRET);
		await page.keyboard.press('Enter');
		await expect(dialog.getByTestId('consent-problem')).toContainText('stays off');
		await expect(key).toHaveValue('');
		await expect(dialog).not.toContainText(SECRET);
		expect(creds).toHaveLength(1);
		expect(enables).toEqual([{ enabled: true }]);
		await expect(toggle).not.toBeChecked();
		await page.keyboard.press('Escape');
		await expect(dialog).toBeHidden();
		await expect(toggle).toBeFocused();
	});

	test('Purge shows the counts before the password is asked', async ({ page, go, kbd }) => {
		await go('/settings/sources');
		await kbd.tabTo(page.getByTestId('source-5-purge'), 60);
		await kbd.press('Enter');
		const dialog = page.getByRole('dialog', { name: 'Purge Private Tracker?' });
		await expect(dialog.getByTestId('preview-torrents')).toContainText('1,204,331');
		await expect(dialog.getByTestId('preview-kept')).toContainText('88,120');
		await expect(dialog.getByTestId('purge-password')).toHaveValue('');
		await expect(dialog.getByTestId('purge-cancel')).toBeFocused();
		await expectNoA11yViolations(page);
		await page.keyboard.press('Escape');
		await expect(dialog).toBeHidden();
	});
});
