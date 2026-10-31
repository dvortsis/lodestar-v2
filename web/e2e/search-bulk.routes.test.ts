// S-search-5 against the OpenAPI mock: selection in the list and the table, the bulk bar (Copy
// magnets, Export, Compare), selection across pages and its reset, and axe. Condition waits only.
import type { Page } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import {
	body,
	copied,
	expect,
	hashOf,
	hits,
	recordClipboard,
	row,
	server
} from './search-results.fixtures';
import { test } from './support/test';

const pick = (page: Page, n: number) => page.getByTestId(`result-${hashOf(n)}-select`);
const count = (page: Page) => page.getByTestId('bulk-count');

test.beforeEach(({ mock }) => server(mock, () => body(hits(8), { total: 8 })));

test('Select mode shows checkboxes in the list; the bar counts and clears', async ({
	page,
	go
}) => {
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	await expect(pick(page, 1)).toHaveCount(0);
	await page.getByTestId('select-mode').click();
	await pick(page, 1).check();
	await expect(count(page)).toHaveText('1 selected');
	await expect(page.getByTestId('results-announce')).toHaveText('1 selected');
	// Ticking a box doesn't open the drawer.
	await expect(page).not.toHaveURL(/\/t\//);
	await pick(page, 3).click({ modifiers: ['Shift'] });
	await expect(count(page)).toHaveText('3 selected');
	await page.getByTestId('bulk-clear').click();
	await expect(page.getByTestId('bulk-bar')).toHaveCount(0);
	await expect(pick(page, 1)).toHaveCount(0);
});

test('Compare is enabled for two to five and goes to /compare', async ({ page, go, mock }) => {
	mock.error('compareTorrents', 500, 'unavailable'); // the route's own spec covers the page
	await go('/search?q=lantern&density=compact');
	await page.getByTestId('select-mode').click();
	await pick(page, 1).check();
	await expect(page.getByTestId('bulk-compare')).toBeDisabled();
	await expect(page.getByTestId('bulk-compare-hint')).toBeVisible();
	for (const n of [2, 3, 4, 5]) await pick(page, n).check();
	await expect(page.getByTestId('bulk-compare')).toBeEnabled();
	await pick(page, 6).check();
	await expect(page.getByTestId('bulk-compare')).toBeDisabled();
	await pick(page, 6).uncheck();
	await page.getByTestId('bulk-compare').click();
	await expect(page).toHaveURL(/\/compare\?/);
	const h = new URL(page.url()).searchParams.get('h')!.split(',');
	expect(h).toEqual([1, 2, 3, 4, 5].map(hashOf));
});

test('Copy magnets sends the selected hashes and copies what the server built', async ({
	page,
	go
}) => {
	await recordClipboard(page);
	let sent: unknown;
	await page.route('**/api/v1/export', async (route) => {
		sent = route.request().postDataJSON();
		await route.fulfill({
			status: 200,
			contentType: 'text/plain',
			body: `magnet:?xt=urn:btih:${hashOf(1)}\nmagnet:?xt=urn:btih:${hashOf(2)}\n`
		});
	});
	await go('/search?q=lantern&view=table');
	await page.getByTestId('table-row-1').focus();
	await page.getByTestId('select-mode').click();
	await page.getByTestId('table-select-1').check();
	await page.getByTestId('table-select-2').check();
	await page.getByTestId('bulk-copy').click();
	await expect(page.getByText('Copied 2 magnet links.')).toBeVisible();
	expect(sent).toEqual({ hashes: [hashOf(1), hashOf(2)], format: 'magnets' });
	expect(await copied(page)).toBe(
		`magnet:?xt=urn:btih:${hashOf(1)}\nmagnet:?xt=urn:btih:${hashOf(2)}`
	);
});

test('a failed export says so and keeps the selection', async ({ page, go }) => {
	await page.route('**/api/v1/export', (route) => route.fulfill({ status: 500, body: '{}' }));
	await go('/search?q=lantern');
	await page.getByTestId('select-mode').click();
	await pick(page, 1).check();
	await page.getByTestId('bulk-export').click();
	await expect(page.getByText(/Couldn.t reach Lodestar/)).toBeVisible();
	await expect(count(page)).toHaveText('1 selected');
});

test('Export downloads the file the server built', async ({ page, go }) => {
	await page.route('**/api/v1/export', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'text/csv',
			headers: { 'Content-Disposition': 'attachment; filename="picked.csv"' },
			body: 'hash,name\n'
		})
	);
	await go('/search?q=lantern');
	await page.getByTestId('select-mode').click();
	await pick(page, 1).check();
	const download = page.waitForEvent('download');
	await page.getByTestId('bulk-export').click();
	expect((await download).suggestedFilename()).toBe('picked.csv');
});

test('the selection survives the next page and clears on a new query', async ({
	page,
	go,
	mock
}) => {
	server(mock, (sent) =>
		sent.cursor === 'p2'
			? body(hits(2, 50), { total: 52 })
			: body(hits(50), { next_cursor: 'p2', total: 52 })
	);
	await go('/search?q=lantern&view=table');
	await page.getByTestId('select-mode').click();
	await page.getByTestId('table-select-1').check();
	await page.getByTestId('table-sentinel').scrollIntoViewIfNeeded();
	await expect(page.getByTestId('table-row-52')).toHaveCount(1);
	await expect(count(page)).toHaveText('1 selected');
	await expect(page.getByTestId('table-select-1')).toBeChecked();
	// A new query starts over.
	await go('/search?q=other&view=table');
	await expect(page.getByTestId('table-row-1')).toBeVisible();
	await expect(page.getByTestId('bulk-bar')).toHaveCount(0);
	await expect(page.getByTestId('table-select-1')).toHaveCount(0);
});

for (const mode of ['light', 'dark'] as const) {
	test.describe(`axe, Riverstone ${mode}`, () => {
		test.use({ themeId: `riverstone-${mode}`, colorMode: mode });
		for (const width of [1366, 390]) {
			test(`the bulk bar over the list and the table at ${width} px`, async ({ page, go }) => {
				await page.setViewportSize({ width, height: 900 });
				await go('/search?q=lantern');
				await page.getByTestId('select-mode').click();
				await pick(page, 1).check();
				await expect(page.getByTestId('bulk-bar')).toBeVisible();
				await expectNoA11yViolations(page);
				await go('/search?q=lantern&view=table');
				await page.getByTestId('select-mode').click();
				await page.getByTestId('table-select-1').check();
				await expectNoA11yViolations(page);
			});
		}
	});
}
