// Keyboard-only M-auth flows (no pointer): first-run setup end to end, and sign-in.
import { expect, test } from './support/test';
import { crawlerFixture, overviewFixture } from '../src/lib/features/insights/overview/fixtures';

test('keyboard: first-run setup from token to /welcome', async ({ page, go, mock, kbd }) => {
	mock.on('getAuthStatus', { logged_in: false, setup_required: true });
	mock.on('setup', () => {
		mock.on('getAuthStatus', { logged_in: true, setup_required: false });
		return { ok: true };
	});
	await go('/setup');
	await kbd.tabTo(page.getByLabel('Setup token'));
	await page.keyboard.type('ab12cd34ef567890');
	await kbd.press('Enter');
	await expect(page.getByLabel('New admin password')).toBeFocused();
	await page.keyboard.type('correct horse battery');
	await kbd.press('Tab');
	await expect(page.getByLabel('Confirm password')).toBeFocused();
	await page.keyboard.type('correct horse battery');
	await kbd.press('Enter');
	await expect(page).toHaveURL(/\/welcome/);
	await expect(page.getByRole('heading', { level: 1, name: 'Finish setting up' })).toBeFocused();
	await expect(page.getByRole('button', { name: 'Retest' })).toBeVisible();
	await kbd.press('Tab');
	await expect(page.getByRole('button', { name: 'Retest' })).toBeFocused();
	expect(mock.calls('setup')[0]!.postDataJSON()).toEqual({
		token: 'AB12-CD34-EF56-7890',
		password: 'correct horse battery'
	});
});

test('keyboard: the token help opens and Back returns to step 1', async ({
	page,
	go,
	mock,
	kbd
}) => {
	mock.on('getAuthStatus', { logged_in: false, setup_required: true });
	await go('/setup');
	await kbd.tabTo(page.getByText('Where to find the log'));
	await kbd.press('Enter');
	await expect(page.getByText('docker logs lodestar')).toBeVisible();
	await kbd.tabTo(page.getByLabel('Setup token'), 20);
	await page.keyboard.type('AB12-CD34-EF56-7890');
	await kbd.press('Enter');
	await kbd.tabTo(page.getByRole('button', { name: 'Back' }));
	await kbd.press('Enter');
	await expect(page.getByLabel('Setup token')).toBeFocused();
});

test('keyboard: sign in and return to next', async ({ page, go, mock, kbd }) => {
	mock.on('getAuthStatus', { logged_in: false, setup_required: false });
	mock.on('login', { ok: true });
	mock.on('getInsightsOverview', overviewFixture());
	mock.on('getCrawler', crawlerFixture());
	await go(`/login?next=${encodeURIComponent('/insights')}`);
	await kbd.tabTo(page.getByLabel('Password'));
	await page.keyboard.type('correct horse battery');
	await kbd.press('Enter');
	await expect(page).toHaveURL(/\/insights/);
});
