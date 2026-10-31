// Cards density (S-search-6; §10.4): the same hits and actions as cards, one to three columns by
// the list's width, opening one, and axe.
import { expectNoA11yViolations } from './a11y';
import { body, expect, hits, row, server } from './search-results.fixtures';
import { test } from './support/test';

for (const [width, cols] of [
	[390, 1],
	[760, 2],
	[1600, 3]
] as const) {
	test(`cards at ${width}px lay out in ${cols} column(s)`, async ({ page, mock }) => {
		await page.setViewportSize({ width, height: 900 });
		server(mock, () => body(hits(9)));
		await page.goto('/search?q=lantern&density=cards');
		await expect(row(page, 1)).toBeVisible();
		await expect(row(page, 1).getByTestId(`result-${'a'.repeat(39)}1-size`)).toBeVisible();
		await expect
			.poll(() =>
				page
					.locator('.cd-row')
					.first()
					.evaluate((el) => getComputedStyle(el).gridTemplateColumns.split(' ').length)
			)
			.toBe(cols);
		await expectNoA11yViolations(page);
	});
}

test('a tile opens the drawer with Enter and the density control switches views', async ({
	page,
	mock
}) => {
	await page.setViewportSize({ width: 1600, height: 900 });
	server(mock, () => body(hits(4)));
	await page.goto('/search?q=lantern&density=cards');
	await row(page, 1).focus();
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL(/\/t\//);
	await page.goto('/search?q=lantern&density=cards');
	await expect(row(page, 1)).toBeVisible();
	await page.getByRole('radio', { name: 'Compact' }).check();
	await expect.poll(() => new URL(page.url()).searchParams.get('density')).toBe('compact');
	await expect(page.locator('.cd-row')).toHaveCount(0);
});

test('arrows and j/k move one tile at a time, and the drawer opens the focused tile', async ({
	page,
	mock
}) => {
	await page.setViewportSize({ width: 1600, height: 900 });
	server(mock, () => body(hits(7)));
	await page.goto('/search?q=lantern&density=cards');
	await expect(row(page, 1)).toBeVisible();
	await row(page, 1).focus();
	await page.keyboard.press('ArrowRight');
	await expect(row(page, 2)).toBeFocused();
	await page.keyboard.press('ArrowDown');
	await expect(row(page, 5)).toBeFocused();
	await page.keyboard.press('ArrowLeft');
	await expect(row(page, 4)).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL(new RegExp(`/t/${'a'.repeat(39)}4`));
});
