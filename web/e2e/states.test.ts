// §10.17 brand gate: a visual snapshot of an illustrated full-page state in light and dark at each
// breakpoint. Runs in the `bp<w>-riverstone-<mode>-comfortable` matrix projects (Compact repeats
// the same pixels, so it only checks the page renders). The not-found state is the one every
// install can reach; the shell tests cover the illustration's markup.
import { expectNoA11yViolations } from './a11y';
import { expectPageSnapshot } from './snapshot';
import { expect as baseExpect, test } from './support/test';

const expect = baseExpect.configure({ timeout: 10_000 });

test('illustrated state: not found', async ({ page, go, density }) => {
	await go('/definitely-not-a-page');
	await expect(page.getByRole('heading', { level: 1, name: 'Page not found' })).toBeVisible();
	// The scene follows the theme's mode; wait for it only where the viewport leaves room (>= 600 px).
	if ((page.viewportSize()?.height ?? 0) >= 600)
		await expect(page.locator('[data-testid="state-screen"] img')).toBeVisible();
	await expectNoA11yViolations(page);
	if (density === 'comfortable') await expectPageSnapshot(page, 'state-not-found');
});
