// Chart primitives showcase (W7e): axe in Riverstone light and dark, and the histogram brush
// driven by the keyboard alone. Lives in the keyboard-only project (no pointer events allowed).
import { expectNoA11yViolations } from './a11y';
import { expect, test } from './support/test';

for (const mode of ['light', 'dark'] as const) {
	test.describe(`riverstone ${mode}`, () => {
		test.use({ themeId: `riverstone-${mode}`, colorMode: mode });

		test('charts showcase has no axe violations', async ({ page, go, kbd }) => {
			await go('/dev/ui/charts');
			await expect(page.getByTestId('line')).toHaveAttribute('data-ready', 'true');
			await expectNoA11yViolations(page);
			await page.getByTestId('line-toggle').focus();
			await kbd.press('Enter');
			await expect(page.getByTestId('line-table')).not.toHaveClass(/sr-only/);
			await expectNoA11yViolations(page);
		});
	});
}

// The classes must resolve to real colours: a utility Tailwind never generated computes to
// transparent / inherited and the brush band, bar track and muted text silently vanish.
for (const mode of ['light', 'dark'] as const) {
	test.describe(`computed styles, riverstone ${mode}`, () => {
		test.use({ themeId: `riverstone-${mode}`, colorMode: mode });

		test('selection band and muted text use their tokens', async ({ page, go, kbd }) => {
			await go('/dev/ui/charts');
			await page.getByRole('group', { name: 'Torrents by size' }).focus();
			await kbd.press('Enter');
			const resolve = (prop: 'background-color' | 'color', token: string) =>
				page.evaluate(
					([p, t]) => {
						const probe = document.createElement('div');
						probe.style.setProperty(p!, `var(${t})`);
						document.body.append(probe);
						const value = getComputedStyle(probe).getPropertyValue(p!);
						probe.remove();
						return value;
					},
					[prop, token]
				);
			const band = await page
				.getByTestId('hist-selection')
				.evaluate((el) => getComputedStyle(el).backgroundColor);
			expect(band).toBe(await resolve('background-color', '--color-selected'));
			expect(band).not.toBe('rgba(0, 0, 0, 0)');
			const muted = await page
				.getByTestId('hbars')
				.locator('[data-testid="hbars-share"]')
				.first()
				.evaluate((el) => getComputedStyle(el).color);
			expect(muted).toBe(await resolve('color', '--color-muted-text'));
			const track = await page
				.getByTestId('hbars-item')
				.first()
				.locator('div[aria-hidden="true"]')
				.evaluate((el) => getComputedStyle(el).backgroundColor);
			expect(track).toBe(await resolve('background-color', '--color-selected'));
		});
	});
}

test('histogram brush works with the keyboard', async ({ page, go, kbd }) => {
	await go('/dev/ui/charts');
	const plot = page.getByRole('group', { name: 'Torrents by size' });
	await plot.focus();
	await kbd.press('Enter');
	const sel = page.getByTestId('hist-range');
	await expect(sel).toHaveAttribute('data-range', '0-8');
	await kbd.press('Tab');
	await expect(page.getByRole('slider', { name: 'Smallest size' })).toBeFocused();
	await kbd.press('ArrowRight');
	await kbd.press('Tab');
	await kbd.press('ArrowLeft');
	await expect(sel).toHaveAttribute('data-range', '1-7');
	await kbd.press('Escape');
	await expect(sel).toHaveAttribute('data-range', 'none');
	await expect(plot).toBeFocused();
});

test('the line chart waits for uPlot and shows the unit', async ({ page, go }) => {
	await go('/dev/ui/charts');
	await expect(page.getByTestId('line')).toHaveAttribute('data-ready', 'true');
	await expect(page.getByTestId('line').locator('canvas')).toHaveCount(1);
	const legend = page.getByTestId('line').locator('.u-legend');
	await expect(legend).toContainText('Seeders');
	await expect(legend).toContainText('Date');
	await expect(legend).not.toContainText('Time');
});
