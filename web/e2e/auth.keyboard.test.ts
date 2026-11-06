// Keyboard-only M-auth flows (no pointer): first-run setup end to end, and sign-in.
import { expect, test } from './support/test';
import { crawlerFixture, overviewFixture } from '../src/lib/features/insights/overview/fixtures';

test('keyboard: first-run setup from the Legal Notice to /welcome', async ({
	page,
	go,
	mock,
	kbd
}) => {
	mock.on('getAuthStatus', { logged_in: false, notice_required: false, setup_required: true });
	mock.on('setup', () => {
		mock.on('getAuthStatus', { logged_in: true, notice_required: false, setup_required: false });
		return { ok: true };
	});
	await go('/setup');
	// The Legal Notice comes first: the region is focusable so it can be scrolled; its links, then
	// the checkbox, follow.
	await kbd.tabTo(page.getByRole('region', { name: 'Legal Notice' }));
	await kbd.tabTo(page.getByLabel('I have read the Legal Notice'), 20);
	await kbd.press('Space');
	await kbd.tabTo(page.getByRole('button', { name: 'Continue' }));
	await kbd.press('Enter');
	await expect(page.getByLabel('Setup token')).toBeFocused();
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
		password: 'correct horse battery',
		notice_version: 4
	});
});

test('keyboard: the token help opens, and Back returns to the Legal Notice and keeps the token', async ({
	page,
	go,
	mock,
	kbd
}) => {
	mock.on('getAuthStatus', { logged_in: false, notice_required: false, setup_required: true });
	await go('/setup');
	await kbd.tabTo(page.getByLabel('I have read the Legal Notice'), 20);
	await kbd.press('Space');
	await kbd.tabTo(page.getByRole('button', { name: 'Continue' }));
	await kbd.press('Enter');
	await expect(page.getByLabel('Setup token')).toBeFocused();
	await kbd.tabTo(page.getByText('Where to find the log'));
	await kbd.press('Enter');
	await expect(page.getByText('docker logs lodestar')).toBeVisible();
	await kbd.tabTo(page.getByLabel('Setup token'), 20);
	await page.keyboard.type('AB12-CD34-EF56-7890');
	await kbd.press('Enter');
	await expect(page.getByLabel('New admin password')).toBeFocused();
	await kbd.tabTo(page.getByRole('button', { name: 'Back' }));
	await kbd.press('Enter');
	await expect(page.getByLabel('Setup token')).toBeFocused();
	await expect(page.getByLabel('Setup token')).toHaveValue('AB12-CD34-EF56-7890');
	await kbd.tabTo(page.getByRole('button', { name: 'Back' }));
	await kbd.press('Enter');
	await expect(page.getByRole('region', { name: 'Legal Notice' })).toBeFocused();
	await expect(page.getByLabel('I have read the Legal Notice')).toBeChecked();
});

test('keyboard: sign in and return to next', async ({ page, go, mock, kbd }) => {
	mock.on('getAuthStatus', { logged_in: false, notice_required: false, setup_required: false });
	mock.on('login', { ok: true });
	mock.on('getInsightsOverview', overviewFixture());
	mock.on('getCrawler', crawlerFixture());
	await go(`/login?next=${encodeURIComponent('/insights')}`);
	await kbd.tabTo(page.getByLabel('Password'));
	await page.keyboard.type('correct horse battery');
	await kbd.press('Enter');
	await expect(page).toHaveURL(/\/insights/);
});
