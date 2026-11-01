// Smoke test of every route, run by each matrix project (breakpoint × theme × density), the
// theme-sample projects and the reduced-motion project: it renders, applies the project's theme,
// has no axe violations and, for the routes the snapshot policy names, matches its baseline.
import { expectNoA11yViolations } from './a11y';
import { snapshotName } from './support/policy';
import { ROUTES } from './support/routes';
import { expectPageSnapshot } from './snapshot';
import { expect, test } from './support/test';

for (const route of ROUTES) {
	test(`smoke ${route.name}`, async ({ page, go, mock, themeId, colorMode, density, shots }) => {
		route.mock?.(mock);
		await go(route.path);
		await expect(page.locator('html')).toHaveAttribute('data-theme', themeId);
		await expect(page.locator('html')).toHaveCSS('color-scheme', colorMode);
		if (route.heading)
			await expect(page.getByRole('heading', { name: route.heading })).toBeVisible();
		await expectNoA11yViolations(page);
		const name = snapshotName(route, { shots, density, themeId });
		if (name) await expectPageSnapshot(page, name);
	});
}

// Under a coarse pointer every standalone control is at least --target-min (44 px) square (§10.2).
test.describe('touch targets', () => {
	test.use({ hasTouch: true, isMobile: true, viewport: { width: 390, height: 800 } });
	test('the Search bar controls are 44 px under a coarse pointer', async ({ page, go }) => {
		await go('/search');
		await page.getByRole('button', { name: 'Syntax' }).click();
		const dialog = page.getByRole('dialog', { name: 'Search syntax' });
		await expect(dialog).toBeVisible();
		for (const button of [
			page.getByRole('button', { name: 'Syntax' }),
			...(await dialog.getByRole('button').all())
		]) {
			const box = (await button.boundingBox())!;
			expect(box.width).toBeGreaterThanOrEqual(44);
			expect(box.height).toBeGreaterThanOrEqual(44);
		}
	});
});
