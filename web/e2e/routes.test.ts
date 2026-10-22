import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';

test('/ redirects to /search', async ({ page }) => {
	await page.goto('/');
	await expect(page).toHaveURL(/\/search$/);
	await expect(page.getByRole('heading', { name: 'Search' })).toBeVisible();
});

test('/search has no axe violations', async ({ page }) => {
	await page.goto('/search');
	const results = await new AxeBuilder({
		page: page as unknown as ConstructorParameters<typeof AxeBuilder>[0]['page']
	}).analyze();
	expect(results.violations).toEqual([]);
});
