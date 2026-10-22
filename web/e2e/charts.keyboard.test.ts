// Chart primitives showcase (W7e): axe in Riverstone light and dark, and the histogram brush
// driven by the keyboard alone. Lives in the keyboard-only project (no pointer events allowed).
import { expectNoA11yViolations } from './a11y';
import { expect, test } from './support/test';

for (const mode of ['light', 'dark'] as const) {
	test.describe(`riverstone ${mode}`, () => {
		test.use({ themeId: `riverstone-${mode}`, colorMode: mode });

		test('charts showcase has no axe violations', async ({ page, go }) => {
			await go('/_dev/ui/charts');
			await expect(page.getByTestId('line')).toHaveAttribute('data-ready', 'true');
			await expectNoA11yViolations(page);
			await page.getByTestId('line-toggle').focus();
			await page.keyboard.press('Enter');
			await expect(page.getByTestId('line-table')).not.toHaveClass(/sr-only/);
			await expectNoA11yViolations(page);
		});
	});
}

test('histogram brush works with the keyboard', async ({ page, go }) => {
	await go('/_dev/ui/charts');
	const plot = page.getByRole('group', { name: 'Torrents by size' });
	await plot.focus();
	await page.keyboard.press('Enter');
	const sel = page.getByTestId('hist-range');
	await expect(sel).toHaveAttribute('data-range', '0-8');
	await page.keyboard.press('Tab');
	await expect(page.getByRole('slider', { name: 'Smallest size' })).toBeFocused();
	await page.keyboard.press('ArrowRight');
	await page.keyboard.press('Tab');
	await page.keyboard.press('ArrowLeft');
	await expect(sel).toHaveAttribute('data-range', '1-7');
	await page.keyboard.press('Escape');
	await expect(sel).toHaveAttribute('data-range', 'none');
	await expect(plot).toBeFocused();
});

test('the line chart waits for uPlot and shows the unit', async ({ page, go }) => {
	await go('/_dev/ui/charts');
	await expect(page.getByTestId('line')).toHaveAttribute('data-ready', 'true');
	await expect(page.getByTestId('line').locator('canvas')).toHaveCount(1);
	await expect(page.getByTestId('line').locator('.u-legend')).toContainText('Seeders');
});
