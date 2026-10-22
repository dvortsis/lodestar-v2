// Nightly: every theme (Riverstone and the 46 others) must pass axe on every route (the compiler
// enforces the §10.18 contrast gates; this is the rendered-page check), and the 46 others match
// their key-screen baselines, the same set the rotating sample projects check 6 at a time.
import { expectNoA11yViolations } from './a11y';
import { snapshotName } from './support/policy';
import { ROUTES } from './support/routes';
import { expectPageSnapshot } from './snapshot';
import { ALL_THEMES } from './support/themes';
import { expect, test } from './support/test';

for (const theme of ALL_THEMES) {
	test.describe(theme.id, () => {
		test.use({ themeId: theme.id, colorMode: theme.mode, shots: 'themes' });
		for (const route of ROUTES) {
			test(`axe ${route.name}`, async ({ page, go, mock, density, shots }) => {
				route.mock?.(mock);
				await go(route.path);
				await expect(page.locator('html')).toHaveAttribute('data-theme', theme.id);
				await expectNoA11yViolations(page);
				const name = snapshotName(route, { shots, density, themeId: theme.id });
				if (name) await expectPageSnapshot(page, name);
			});
		}
	});
}
