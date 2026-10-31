// S-search-5 keyboard-only: table mode's header sort, row focus and Enter, `x` and the bulk bar.
// No pointer events are allowed (the kbd fixture fails the test).
import { body, expect, hashOf, hits, server } from './search-results.fixtures';
import { test } from './support/test';

test.beforeEach(({ mock }) => server(mock, () => body(hits(5), { total: 5 })));

test('header sort from the keys, then rows with the arrows and j/k', async ({ page, go, kbd }) => {
	await page.setViewportSize({ width: 1366, height: 900 });
	await go('/search?q=lantern&view=table');
	await expect(page.getByTestId('table-row-1')).toBeVisible();
	await kbd.tabTo(page.getByTestId('table-th-size').getByRole('button'));
	await kbd.press('Enter');
	await expect(page).toHaveURL(/sort=-size/);
	await expect(page.getByTestId('table-th-size')).toHaveAttribute('aria-sort', 'descending');

	await kbd.tabTo(page.getByTestId('table-row-1'));
	await kbd.press('ArrowDown');
	await expect(page.getByTestId('table-row-2')).toBeFocused();
	await kbd.press('j');
	await expect(page.getByTestId('table-row-3')).toBeFocused();
	await kbd.press('k');
	await expect(page.getByTestId('table-row-2')).toBeFocused();
	await kbd.press('End');
	await expect(page.getByTestId('table-row-5')).toBeFocused();
	await kbd.press('Enter');
	await expect(page).toHaveURL(new RegExp(`/t/${hashOf(5)}`));
});

test('x selects the focused row and the bulk bar appears', async ({ page, go, kbd }) => {
	await go('/search?q=lantern&view=table');
	await kbd.tabTo(page.getByTestId('table-row-1'));
	await kbd.press('x');
	await expect(page.getByTestId('bulk-count')).toHaveText('1 selected');
	await expect(page.getByTestId('table-select-1')).toBeChecked();
	await expect(page.getByTestId('bulk-compare')).toBeDisabled();
	await kbd.press('ArrowDown');
	await kbd.press('x');
	await expect(page.getByTestId('bulk-count')).toHaveText('2 selected');
	await expect(page.getByTestId('bulk-compare')).toBeEnabled();
	await expect(page.getByTestId('results-announce')).toHaveText('2 selected');
	await kbd.press('x');
	await expect(page.getByTestId('bulk-count')).toHaveText('1 selected');
});
