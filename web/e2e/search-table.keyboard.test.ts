// S-search-5 keyboard-only: table mode's header sort, row focus and Enter, `x` and the bulk bar.
// No pointer events are allowed (the kbd fixture fails the test).
import type { Request } from '@playwright/test';
import { type Ast, body, expect, hashOf, hits, print, server } from './search-results.fixtures';
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
	// j/k load lazily and a key pressed before they register is dropped.
	await expect(page.locator('[data-keys-ready]')).toBeAttached();

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

// The flake behind X-flake-search-keyboard-2: the re-sorted rows landed after ArrowDown had moved
// focus, the table reset its active row to the first, and j then went to row 2 again. Hold the
// sorted answer until focus has moved, so the rows always land late.
test('a re-sort that lands after the arrows keeps j on the focused row', async ({
	page,
	go,
	kbd,
	mock
}) => {
	await page.setViewportSize({ width: 1366, height: 900 });
	await go('/search?q=lantern&view=table');
	await expect(page.getByTestId('table-row-1')).toBeVisible();
	await expect(page.locator('[data-keys-ready]')).toBeAttached();
	let asked = false;
	let release!: () => void;
	const gate = new Promise<void>((r) => (release = r));
	mock.on('searchPost', async (request: Request) => {
		const sent = request.postDataJSON() as { ast: Ast; limit: number; sort?: string };
		if (sent.limit === 1) return body([], { canonical: print(sent.ast) });
		if (sent.sort?.includes('size')) {
			asked = true;
			await gate;
		}
		return body(hits(5), { total: 5 });
	});

	await kbd.tabTo(page.getByTestId('table-th-size').getByRole('button'));
	await kbd.press('Enter');
	await expect.poll(() => asked).toBe(true);
	try {
		await kbd.tabTo(page.getByTestId('table-row-1'));
		await kbd.press('ArrowDown');
		await expect(page.getByTestId('table-row-2')).toBeFocused();
	} finally {
		release(); // never leave the sorted request hanging into teardown
	}
	await expect(page.getByTestId('results-refining')).toHaveCount(0);
	await expect(page.getByTestId('table-row-2')).toBeFocused();
	await expect(page.getByTestId('table-row-2')).toHaveAttribute('tabindex', '0');
	await kbd.press('j');
	await expect(page.getByTestId('table-row-3')).toBeFocused();
	await kbd.press('k');
	await expect(page.getByTestId('table-row-2')).toBeFocused();
});

test('x selects the focused row and the bulk bar appears', async ({ page, go, kbd }) => {
	await go('/search?q=lantern&view=table');
	await expect(page.locator('[data-keys-ready]')).toBeAttached();
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
