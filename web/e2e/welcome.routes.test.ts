import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';

const expect = baseExpect.configure({ timeout: 10_000 });

for (const width of [390, 1366]) {
	test(`welcome refresh and actions at ${width}`, async ({ page, go, mock }) => {
		await page.setViewportSize({ width, height: 844 });
		mock.on('getAuthStatus', { logged_in: true, setup_required: false });
		await go('/welcome');
		await expect(page.getByRole('heading', { name: 'Setup complete' })).toBeVisible();
		await page.reload();
		await expect(page.getByRole('heading', { name: 'Setup complete' })).toBeVisible();
		await expectNoA11yViolations(page);
		await page.getByRole('link', { name: 'Go to Search' }).click();
		await expect(page).toHaveURL(/\/search(?:\?|$)/);
		await go('/welcome');
		await page.getByRole('link', { name: 'Review settings' }).click();
		await expect(page).toHaveURL(/\/settings(?:\/|\?|$)/);
	});
}

for (const setup of [true, false]) {
	test(`direct welcome visit without a session (setup: ${setup})`, async ({ page, go, mock }) => {
		mock.on('getAuthStatus', { logged_in: false, setup_required: setup });
		await go('/welcome');
		await expect(page).toHaveURL(setup ? /\/setup(?:\?|$)/ : /\/login\?next=/);
		await expect(page.getByText('Setup complete')).toHaveCount(0);
	});
}
