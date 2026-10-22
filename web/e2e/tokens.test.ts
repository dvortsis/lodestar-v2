import { expect, test } from '@playwright/test';

// The showcase route is test-only (served because the Playwright build sets VITE_DEV_ROUTES).
for (const scheme of ['light', 'dark'] as const) {
	test(`token showcase, Riverstone ${scheme}`, async ({ page }) => {
		await page.emulateMedia({ colorScheme: scheme, reducedMotion: 'reduce' });
		await page.goto('/_dev/tokens');
		await expect(page.locator('html')).toHaveAttribute('data-theme', `riverstone-${scheme}`);
		await page.evaluate(() => document.fonts.ready);
		await expect(page).toHaveScreenshot(`tokens-${scheme}.png`, { fullPage: true });
	});
}

test('daisyUI controls match the control-height tokens', async ({ page }) => {
	await page.goto('/_dev/tokens');
	const height = (id: string) =>
		page.getByTestId(id).evaluate((el) => el.getBoundingClientRect().height);
	expect(await height('control-md')).toBe(40);
	expect(await height('input-md')).toBe(40);
	expect(await height('control-sm')).toBe(32);
	expect(await height('input-sm')).toBe(32);
	expect(await height('query-bar')).toBe(48);
});

test('radii, type scale and weights resolve to the tokens', async ({ page }) => {
	await page.goto('/_dev/tokens');
	const css = (id: string, prop: string) =>
		page.getByTestId(id).evaluate((el, p) => getComputedStyle(el).getPropertyValue(p), prop);
	expect(await css('radius-sheet', 'border-top-left-radius')).toBe('16px');
	expect(await css('radius-box', 'border-top-left-radius')).toBe('12px');
	expect(await css('radius-field', 'border-top-left-radius')).toBe('8px');
	expect(await css('radius-selector', 'border-top-left-radius')).toBe('4px');
	const sample = (id: string) => page.getByTestId(`type-${id}`).locator('span').last();
	const metrics = (id: string) =>
		sample(id).evaluate((el) => {
			const s = getComputedStyle(el);
			return [s.fontSize, s.lineHeight];
		});
	expect(await metrics('caption')).toEqual(['12px', '16px']);
	expect(await metrics('small')).toEqual(['13px', '18px']);
	expect(await metrics('body')).toEqual(['14px', '20px']);
	expect(await metrics('body-lg')).toEqual(['16px', '24px']);
	expect(await metrics('title')).toEqual(['20px', '26px']);
	expect(await metrics('display')).toEqual(['28px', '32px']);
	expect(await css('weight-medium', 'font-weight')).toBe('500');
	expect(await css('weight-semibold', 'font-weight')).toBe('600');
});

test('tabular numerals align digits; the UI font is the loaded face', async ({ page }) => {
	await page.goto('/_dev/tokens');
	await page.evaluate(() => document.fonts.ready);
	const widths = await page.getByTestId('tabular').evaluate((el) => {
		const range = (node: Element) => {
			const r = document.createRange();
			r.selectNodeContents(node);
			return r.getBoundingClientRect().width;
		};
		return [...el.children].map(range);
	});
	expect(widths[0]).toBeCloseTo(widths[1], 1);
	const loaded = await page.evaluate(() =>
		document.fonts.check('14px "Atkinson Hyperlegible Next"')
	);
	expect(loaded).toBe(true);
});
