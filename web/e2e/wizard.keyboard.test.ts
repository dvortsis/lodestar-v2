// The first-run wizard (S-wizard, part 1) at /welcome against the OpenAPI mock: the whole path by
// keyboard alone, with axe on each step, Skip sending nothing, a save and a reload that resumes.
import { SettingsBackend } from '../src/lib/features/settings/form/testing/e2e-backend';
import {
	WIZARD_INDEX,
	lockedUpdates,
	wizardValues
} from '../src/lib/features/wizard/testing/fixtures';
import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';

const expect = baseExpect.configure({ timeout: 10_000 });
const h2 = (page: import('@playwright/test').Page, name: string) =>
	page.getByRole('heading', { level: 2, name });

test.describe('first-run wizard', () => {
	let backend: SettingsBackend;
	test.beforeEach(async ({ mock }) => {
		backend = new SettingsBackend(WIZARD_INDEX, wizardValues()).attach(mock);
		mock.on('getAuthStatus', { logged_in: true, setup_required: false });
	});

	test('keyboard only: every step, Skip, save, Done', async ({ page, go, kbd }) => {
		await go('/welcome');
		await expect(h2(page, 'Network check')).toBeVisible();
		await expect(page.getByText('Reachable', { exact: true }).first()).toBeVisible();
		await expectNoA11yViolations(page);

		// The page heading has focus after the auth check; Tab reaches Retest, then Continue.
		await kbd.tabTo(page.getByRole('button', { name: 'Continue' }));
		await kbd.press('Enter');
		await expect(h2(page, 'How it works')).toBeFocused();
		await expect(page.getByTestId('wizard-how-cold')).toContainText('days and weeks');
		await expectNoA11yViolations(page);

		await kbd.tabTo(page.getByRole('button', { name: 'Continue' }));
		await kbd.press('Enter');
		await expect(h2(page, 'Crawl intensity')).toBeFocused();
		await expect(page.getByRole('radio', { name: 'balanced' })).toBeChecked();
		await expectNoA11yViolations(page);
		await kbd.tabTo(page.getByRole('radio', { name: 'balanced' }));
		await kbd.press('ArrowLeft');
		await expect(page.getByRole('radio', { name: 'low' })).toBeChecked();
		await kbd.tabTo(page.getByRole('button', { name: 'Save and continue' }));
		await kbd.press('Enter');
		await expect(h2(page, 'Storage & retention')).toBeFocused();
		expect(backend.patches).toEqual([{ crawler: { if_revision: 3, intensity: 'low' } }]);
		await expectNoA11yViolations(page);

		// Skip sends nothing.
		await kbd.tabTo(page.getByRole('button', { name: 'Skip this step' }));
		await kbd.press('Enter');
		await expect(h2(page, 'Updates')).toBeFocused();
		expect(backend.patches).toHaveLength(1);
		await expect(page.getByRole('radio', { name: 'Off' })).toBeChecked();
		await expect(page.getByTestId('wizard-updates-github')).toContainText('GitHub');
		await expectNoA11yViolations(page);

		await kbd.tabTo(page.getByRole('radio', { name: 'Off' }));
		await kbd.press('ArrowDown');
		await expect(page.getByRole('radio', { name: 'Check and notify' })).toBeChecked();
		await kbd.tabTo(page.getByRole('button', { name: 'Save and continue' }));
		await kbd.press('Enter');
		await expect(h2(page, 'Done')).toBeFocused();
		expect(backend.patches.at(-1)).toEqual({ updates: { if_revision: 1, mode: 'notify' } });
		await expect(
			page.getByTestId('wizard-skipped').getByRole('link', { name: 'Storage & retention' })
		).toBeVisible();
		await expectNoA11yViolations(page);
		await kbd.tabTo(page.getByRole('link', { name: 'Go to Search' }));
		await kbd.press('Enter');
		await expect(page).toHaveURL(/\/search(?:\?|$)/);
	});

	test('a reload resumes the step; nothing is saved by moving around', async ({ page, go }) => {
		await go('/welcome');
		await page.getByRole('button', { name: 'Continue' }).click();
		await page.getByRole('button', { name: 'Continue' }).click();
		await expect(h2(page, 'Crawl intensity')).toBeVisible();
		await page.reload();
		await expect(h2(page, 'Crawl intensity')).toBeVisible();
		await page.getByRole('button', { name: 'Back' }).click();
		await expect(h2(page, 'How it works')).toBeVisible();
		expect(backend.patches).toEqual([]);
	});

	test('a value set by the environment is shown locked', async ({ page, go }) => {
		backend.change('updates', { mode: lockedUpdates() });
		await go('/welcome');
		await page.getByRole('button', { name: 'Skip this step' }).click();
		await page.getByRole('button', { name: 'Continue' }).click();
		await page.getByRole('button', { name: 'Skip this step' }).click();
		await page.getByRole('button', { name: 'Skip this step' }).click();
		await expect(h2(page, 'Updates')).toBeVisible();
		await expect(page.getByRole('radio', { name: 'Check and notify' })).toBeChecked();
		await expect(page.getByRole('radio', { name: 'Check and notify' })).toBeDisabled();
		await expect(page.getByTestId('wizard-updates-locked')).toContainText('LODESTAR_UPDATES');
		await expectNoA11yViolations(page);
	});

	test('layout holds at 390 px', async ({ page, go }) => {
		await page.setViewportSize({ width: 390, height: 844 });
		await go('/welcome');
		await expect(h2(page, 'Network check')).toBeVisible();
		const overflow = await page.evaluate(
			() => document.documentElement.scrollWidth - document.documentElement.clientWidth
		);
		expect(overflow).toBeLessThanOrEqual(0);
		await expectNoA11yViolations(page);
	});
});
