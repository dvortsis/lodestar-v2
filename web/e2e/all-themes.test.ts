// Nightly: every theme (Riverstone and the 46 others) must pass axe on every route. The contrast
// gates of §10.18 are enforced by the compiler; this is the rendered-page check. No snapshots here:
// the rotating sample projects own those.
import { expectNoA11yViolations } from './a11y';
import { ROUTES } from './support/routes';
import { ALL_THEMES } from './support/themes';
import { expect, test } from './support/test';

for (const theme of ALL_THEMES) {
	test.describe(theme.id, () => {
		test.use({ themeId: theme.id, colorMode: theme.mode });
		for (const route of ROUTES) {
			test(`axe ${route.name}`, async ({ page, go }) => {
				await go(route.path);
				await expect(page.locator('html')).toHaveAttribute('data-theme', theme.id);
				await expectNoA11yViolations(page);
			});
		}
	});
}
