// Smoke test of every route, run by each matrix project (breakpoint × theme × density), the
// theme-sample projects and the reduced-motion project: it renders, applies the project's theme,
// has no axe violations and matches its visual baseline.
import { expectNoA11yViolations } from './a11y';
import { ROUTES } from './support/routes';
import { expectPageSnapshot } from './snapshot';
import { expect, test } from './support/test';

for (const route of ROUTES) {
	test(`smoke ${route.name}`, async ({ page, go, themeId, colorMode }) => {
		await go(route.path);
		await expect(page.locator('html')).toHaveAttribute('data-theme', themeId);
		await expect(page.locator('html')).toHaveCSS('color-scheme', colorMode);
		if (route.heading)
			await expect(page.getByRole('heading', { name: route.heading })).toBeVisible();
		await expectNoA11yViolations(page);
		await expectPageSnapshot(page, route.name);
	});
}
