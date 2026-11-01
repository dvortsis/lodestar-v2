import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';

const expect = baseExpect.configure({ timeout: 10_000 });

for (const width of [390, 1366]) {
	test(`welcome wizard refresh at ${width}`, async ({ page, go, mock }) => {
		await page.setViewportSize({ width, height: 844 });
		mock.on('getAuthStatus', { logged_in: true, setup_required: false });
		await go('/welcome');
		await expect(page.getByRole('heading', { level: 1, name: 'Finish setting up' })).toBeVisible();
		await page.reload();
		await expect(page.getByRole('heading', { level: 1, name: 'Finish setting up' })).toBeVisible();
		await expect(page.getByRole('heading', { level: 2, name: 'Network check' })).toBeVisible();
		await expectNoA11yViolations(page);
	});
}

for (const setup of [true, false]) {
	test(`direct welcome visit without a session (setup: ${setup})`, async ({ page, go, mock }) => {
		mock.on('getAuthStatus', { logged_in: false, setup_required: setup });
		await go('/welcome');
		await expect(page).toHaveURL(setup ? /\/setup(?:\?|$)/ : /\/login\?next=/);
		await expect(page.getByText('Finish setting up')).toHaveCount(0);
	});
}
