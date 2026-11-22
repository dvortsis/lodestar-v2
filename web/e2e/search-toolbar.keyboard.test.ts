// S-search-3 keyboard-only: "Skip to results" past the bar and the facet rail, and the toolbar's
// density from the keys. No pointer events are allowed (the kbd fixture fails the test).
import { body, expect, hits, row, rowOf, server } from './search-results.fixtures';
import { interact } from './support/interact';
import { test } from './support/test';

test.beforeEach(({ mock }) => server(mock, () => body(hits(5))));

test('Skip to results shows on focus and lands on the list’s active row', async ({
	page,
	go,
	kbd
}) => {
	await page.setViewportSize({ width: 1366, height: 900 });
	await go('/search?q=lantern');
	await expect(rowOf(page, 1)).toBeVisible();
	const skip = page.getByRole('link', { name: 'Skip to results' });
	await kbd.tabTo(skip);
	await expect(skip).toBeVisible();
	await kbd.press('Enter');
	await expect(rowOf(page, 1)).toBeFocused();
	await kbd.press('j');
	await expect(rowOf(page, 2)).toBeFocused();
	// From the bar, it's one Shift+Tab back.
	await kbd.press('/');
	await expect(page.getByRole('combobox', { name: 'Search torrents' })).toBeFocused();
	await kbd.press('Shift+Tab');
	await expect(skip).toBeFocused();
	await kbd.press('Enter');
	await expect(rowOf(page, 2)).toBeFocused();
});

test('with no list yet, Skip to results goes to the results region', async ({ page, go, kbd }) => {
	await go('/search');
	await interact(page);
	await expect(page.getByTestId('results-empty')).toBeVisible();
	await kbd.tabTo(page.getByRole('link', { name: 'Skip to results' }));
	await kbd.press('Enter');
	await expect(page.locator('#results')).toBeFocused();
});

test('the view from the keys: the select changes the rows', async ({ page, go, kbd }) => {
	await go('/search?q=lantern');
	await expect(rowOf(page, 1)).toBeVisible();
	const view = page.getByTestId('view-select');
	await kbd.tabTo(view);
	// Type-ahead picks the option by its first letters, with no popup to open (a native select).
	await kbd.press('c');
	await expect(view).toHaveValue('compact');
	await expect(page).toHaveURL(/density=compact/);
	await expect(row(page, 1)).toHaveAttribute('data-density', 'compact');
});

test('at 390 px with no rows, Skip to results moves focus to the results region', async ({
	page,
	go,
	mock,
	kbd
}) => {
	server(mock, () => ({
		status: 503,
		body: { error: { code: 'unavailable', message: 'down' } }
	}));
	await page.setViewportSize({ width: 390, height: 800 });
	await go('/search?q=lantern');
	await expect(page.getByTestId('results-error')).toBeVisible();
	const skip = page.getByTestId('skip-to-results');
	await kbd.tabTo(skip);
	await kbd.press('Enter');
	await expect(page.locator('#results')).toBeFocused();
});
