// X-first-run-search-gate: the search page, whose bare first load makes no request, still sends a
// fresh instance to setup, a signed-out visitor to sign-in and a pending Legal Notice to the notice,
// as /settings and /insights do. Then the two saves a tester reported lost: Language applies at
// once, and a Notifications route is kept by Save changes.
import { expectNoA11yViolations } from './a11y';
import { mockSettings } from './support/settings-fixture';
import { expect, test } from './support/test';

const status = (logged_in: boolean, notice_required: boolean, setup_required: boolean) => ({
	logged_in,
	notice_required,
	setup_required
});

test('a fresh instance opened at / lands on setup, at the Legal Notice', async ({ page, mock }) => {
	mock.on('getAuthStatus', status(false, false, true));
	await page.goto('/');
	await expect(page).toHaveURL(/\/setup$/);
	await expect(page.getByRole('region', { name: 'Legal Notice' })).toBeVisible();
	await expect(page.getByLabel('I have read the Legal Notice')).toBeVisible();
	await expect(page.getByRole('search')).toHaveCount(0);
	await expectNoA11yViolations(page);
});

test('after an update that needs the new notice, /search lands on the notice and comes back', async ({
	page,
	mock
}) => {
	let accepted = false;
	mock.on('getAuthStatus', () => ({ status: 200, body: status(true, !accepted, false) }));
	mock.on('acceptLegalNotice', () => {
		accepted = true;
		return { status: 200, body: { ok: true } };
	});
	await page.goto('/search');
	await expect(page).toHaveURL(/\/login\?next=%2Fsearch$/);
	await expect(page.getByRole('heading', { name: 'Legal Notice', level: 1 })).toBeVisible();
	await expect(page.getByLabel('Password')).toHaveCount(0);
	await page.getByRole('button', { name: 'I have read it, continue' }).click();
	await expect(page).toHaveURL(/\/search$/);
	await expect(page.getByRole('search')).toBeVisible();
	expect(mock.calls('acceptLegalNotice')).toHaveLength(1);
});

test('a signed-out visitor at /search lands on sign-in', async ({ page, mock }) => {
	mock.on('getAuthStatus', status(false, false, false));
	await page.goto('/search');
	await expect(page).toHaveURL(/\/login\?next=%2Fsearch$/);
	await expect(page.getByLabel('Password')).toBeVisible();
});

test('a signed-in visitor gets the search page, checked once', async ({ page, mock }) => {
	mock.on('getAuthStatus', status(true, false, false));
	await page.goto('/search');
	await expect.poll(() => mock.calls('getAuthStatus').length).toBe(1);
	await expect(page).toHaveURL(/\/search$/);
	await expect(page.getByRole('search')).toBeVisible();
});

const field = (value: unknown) => ({
	value,
	env: null,
	read_only: false,
	restart_required: false,
	source: 'default' as const
});

test('Language applies at once, without Save', async ({ page, go, mock }) => {
	mockSettings(mock);
	mock.on('getSettings', (request) => {
		const section = new URL(request.url()).pathname.split('/').at(-1)!;
		return {
			status: 200,
			body: {
				section,
				revision: 1,
				restart_required: [],
				fields: section === 'preferences' ? { language: field('') } : {}
			}
		};
	});
	mock.on('patchSettingsBatch', {
		status: 200,
		body: {
			sections: {
				preferences: {
					section: 'preferences',
					revision: 2,
					restart_required: [],
					fields: { language: field('en') }
				}
			}
		}
	});
	await go('/settings/language');
	await page.getByRole('switch', { name: 'Language Automatic' }).click();
	await page.getByRole('combobox', { name: 'Language' }).selectOption({ label: 'English' });
	await expect
		.poll(() => mock.calls('patchSettingsBatch').map((r) => r.postDataJSON()))
		.toEqual([{ preferences: { if_revision: 1, language: 'en' } }]);
	await expect(page.getByRole('button', { name: 'Save changes' })).toHaveCount(0);
});

test('a Notifications route is kept by Save changes', async ({ page, go, mock }) => {
	mockSettings(mock);
	mock.on('getSettings', (request) => {
		const section = new URL(request.url()).pathname.split('/').at(-1)!;
		return {
			status: 200,
			body: {
				section,
				revision: 4,
				restart_required: [],
				fields: section === 'notifications' ? { route_alerts: field([]) } : {}
			}
		};
	});
	mock.on('patchSettingsBatch', {
		status: 200,
		body: {
			sections: {
				notifications: {
					section: 'notifications',
					revision: 5,
					restart_required: [],
					fields: { route_alerts: field(['discord']) }
				}
			}
		}
	});
	await go('/settings/notifications');
	const alerts = page.getByRole('group', { name: 'Alerts go to' });
	await alerts.getByRole('checkbox', { name: 'Discord' }).check();
	await page.getByRole('button', { name: 'Save changes' }).click();
	await expect
		.poll(() => mock.calls('patchSettingsBatch').map((r) => r.postDataJSON()))
		.toEqual([{ notifications: { if_revision: 4, route_alerts: ['discord'] } }]);
	await expect(alerts.getByRole('checkbox', { name: 'Discord' })).toBeChecked();
	await expect(page.getByRole('button', { name: 'Save changes' })).toHaveCount(0);
});
