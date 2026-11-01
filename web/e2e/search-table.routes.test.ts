// S-search-5 against the OpenAPI mock: table mode (the view in the URL, header sort, the column
// chooser and its persistence, contained horizontal scroll, paging) and axe. Condition waits only.
import type { Page } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import { body, expect, hits, resultsCalls, server } from './search-results.fixtures';
import { test } from './support/test';

const tableRow = (page: Page, n: number) => page.getByTestId(`table-row-${n}`);
const sortOf = (page: Page) => new URL(page.url()).searchParams.get('sort');

test.beforeEach(({ mock }) => server(mock, () => body(hits(5), { total: 5 })));

test('the View control switches to a real table and keeps it in the URL', async ({ page, go }) => {
	await go('/search?q=lantern');
	await expect(page.getByTestId('result-' + 'a'.repeat(39) + '1')).toBeVisible();
	await page.getByRole('radio', { name: 'Table', exact: true }).click();
	await expect(page).toHaveURL(/view=table/);
	const table = page.getByRole('table', { name: 'Matching torrents' });
	await expect(table).toBeVisible();
	await expect(table.getByRole('columnheader', { name: 'Size' })).toBeVisible();
	await expect(table.getByRole('row')).toHaveCount(6);
	await expect(page.getByTestId('table-title-1')).toHaveText('Lantern Keeper 1');
	await expect(page.getByTestId('table-size-1')).toContainText('GiB');
	await page.getByRole('radio', { name: 'List' }).click();
	await expect(page).not.toHaveURL(/view=/);
	await expect(table).toHaveCount(0);
});

test('a header sorts through the URL, and Back walks it', async ({ page, go, mock }) => {
	await go('/search?q=lantern&view=table');
	const size = page.getByTestId('table-th-size');
	await expect(size).toHaveAttribute('aria-sort', 'none');
	await size.getByRole('button').click();
	await expect.poll(() => sortOf(page)).toBe('-size');
	await expect(size).toHaveAttribute('aria-sort', 'descending');
	await size.getByRole('button').click();
	await expect.poll(() => sortOf(page)).toBe('+size');
	await expect(size).toHaveAttribute('aria-sort', 'ascending');
	// The rows are the server's order: the client never re-sorts a paged list.
	await expect.poll(() => resultsCalls(mock).at(-1)).toBeTruthy();
	expect((mock.calls('searchPost').at(-1)!.postDataJSON() as { sort: string }).sort).toBe('+size');
	await page.goBack();
	await expect(size).toHaveAttribute('aria-sort', 'descending');
	// Columns the server can't sort by are plain headers.
	await expect(page.getByTestId('table-th-type').getByRole('button')).toHaveCount(0);
});

test('the column chooser hides, moves and remembers columns in this browser', async ({
	page,
	go
}) => {
	await go('/search?q=lantern&view=table');
	await expect(page.getByTestId('table-th-files')).toBeVisible();
	await expect(page.getByTestId('table-th-leechers')).toHaveCount(0);
	await page.getByTestId('table-columns').click();
	const chooser = page.getByTestId('table-chooser');
	await chooser.getByTestId('table-col-files').uncheck();
	await chooser.getByTestId('table-col-leechers').check();
	await chooser.getByRole('button', { name: 'Move Type later' }).click();
	await expect(page.getByTestId('table-th-files')).toHaveCount(0);
	await expect(page.getByTestId('table-th-leechers')).toBeVisible();
	await page.keyboard.press('Escape');
	await expect(chooser).toHaveCount(0);
	await expect(page.getByTestId('table-columns')).toBeFocused();

	await page.reload();
	await expect(page.getByTestId('table-th-leechers')).toBeVisible();
	await expect(page.getByTestId('table-th-files')).toHaveCount(0);
	const order = await page
		.locator('thead th')
		.evaluateAll((els) => els.map((el) => (el as HTMLElement).dataset.testid));
	expect(order.indexOf('table-th-size')).toBeLessThan(order.indexOf('table-th-type'));
});

test('corrupt saved columns fall back to the defaults', async ({ page, go }) => {
	await page.addInitScript(() => localStorage.setItem('lodestar.search.columns', '{not json'));
	await go('/search?q=lantern&view=table');
	await expect(page.getByTestId('table-th-files')).toBeVisible();
	await expect(page.getByTestId('table-th-leechers')).toHaveCount(0);
});

test('at a narrow width the table scrolls inside itself, not the page', async ({ page, go }) => {
	await page.setViewportSize({ width: 390, height: 800 });
	await go('/search?q=lantern&view=table');
	await expect(tableRow(page, 1)).toBeVisible();
	const m = await page.evaluate(() => {
		const scroll = document.querySelector('.rt-scroll') as HTMLElement;
		const root = document.documentElement;
		return {
			box: scroll.getBoundingClientRect().width,
			inner: scroll.scrollWidth - scroll.clientWidth,
			page: root.scrollWidth - root.clientWidth
		};
	});
	expect(m.box).toBeLessThanOrEqual(390);
	expect(m.inner).toBeGreaterThan(0);
	expect(m.page).toBeLessThanOrEqual(0);
});

test('the next page arrives from the scroll sentinel', async ({ page, go, mock }) => {
	server(mock, (sent) =>
		sent.cursor === 'p2'
			? body(hits(3, 50), { total: 53 })
			: body(hits(50), { next_cursor: 'p2', total: 53 })
	);
	await go('/search?q=lantern&view=table');
	await expect(tableRow(page, 1)).toBeVisible();
	await page.getByTestId('table-sentinel').scrollIntoViewIfNeeded();
	await expect.poll(() => resultsCalls(mock).some((c) => c.cursor === 'p2')).toBe(true);
	await expect(tableRow(page, 53)).toHaveCount(1);
	await expect(page.getByTestId('results-end')).toBeAttached();
});

test('a new query puts the first row back in the tab order', async ({ page, go, mock }) => {
	server(mock, (sent) =>
		sent.cursor === null && JSON.stringify(sent).includes('-size')
			? body(hits(5), { total: 5 })
			: body(hits(50), { total: 50 })
	);
	await go('/search?q=lantern&view=table');
	await tableRow(page, 40).focus();
	await expect(tableRow(page, 40)).toHaveAttribute('tabindex', '0');
	await page.getByLabel('Sort').selectOption({ label: 'Largest' });
	await expect(tableRow(page, 40)).toHaveCount(0);
	await expect(tableRow(page, 1)).toHaveAttribute('tabindex', '0');
});

for (const mode of ['light', 'dark'] as const) {
	test.describe(`axe, Riverstone ${mode}`, () => {
		test.use({ themeId: `riverstone-${mode}`, colorMode: mode });
		for (const width of [1366, 390]) {
			test(`the table and its chooser at ${width} px`, async ({ page, go }) => {
				await page.setViewportSize({ width, height: 900 });
				await go('/search?q=lantern&view=table');
				await expect(tableRow(page, 1)).toBeVisible();
				await expectNoA11yViolations(page);
				await page.getByTestId('table-columns').click();
				await expect(page.getByTestId('table-chooser')).toBeVisible();
				await expectNoA11yViolations(page);
			});
		}
	});
}

test.describe('under a coarse pointer', () => {
	test.use({ hasTouch: true, isMobile: true, viewport: { width: 390, height: 800 } });
	test('the sort buttons and Reset columns are 44 px targets', async ({ page, go }) => {
		await go('/search?q=lantern&view=table');
		await page.getByTestId('table-columns').click();
		const targets = [
			page.getByTestId('table-th-size').getByRole('button'),
			page.getByTestId('table-th-files').getByRole('button'),
			page.getByTestId('table-chooser').getByRole('button', { name: /reset/i })
		];
		for (const target of targets) {
			const box = (await target.boundingBox())!;
			expect(box.width).toBeGreaterThanOrEqual(44);
			expect(box.height).toBeGreaterThanOrEqual(44);
		}
	});
});
