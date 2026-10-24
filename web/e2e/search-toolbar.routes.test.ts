// S-search-3 against the OpenAPI mock: the results toolbar (count, sort, density, Save search),
// the "Load more" fallback and end line under the list, no layout shift on a pasted link, and axe.
import type { Page } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import { body, expect, hits, resultsCalls, row, server } from './search-results.fixtures';
import { test } from './support/test';

const count = (page: Page) => page.getByTestId('results-count');
const sortOf = (page: Page) => new URL(page.url()).searchParams.get('sort');
const sentSorts = (mock: Parameters<typeof resultsCalls>[0]) =>
	mock
		.calls('searchPost')
		.map((r) => r.postDataJSON() as { limit: number; sort: string | null })
		.filter((b) => b.limit !== 1)
		.map((b) => b.sort);

test('the count is the server’s, ≈ and compact only when it says so', async ({
	page,
	go,
	mock
}) => {
	server(mock, (sent) =>
		JSON.stringify(sent.ast).includes('many')
			? body(hits(3), { total: 2_400_000, total_approx: true })
			: body(hits(3), { total: 1284 })
	);
	await go('/search?q=lantern');
	await expect(count(page)).toHaveText('1,284 results');
	// Visual only: the panel's polite region announces it, once.
	await expect(count(page)).not.toHaveAttribute('aria-live');
	await go('/search?q=many');
	await expect(count(page)).toHaveText('≈ 2.4M results');
});

test('sort goes to the URL and the request; Back walks it', async ({ page, go, mock }) => {
	server(mock, () => body(hits(3)));
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	const sort = page.getByLabel('Sort');
	await expect(sort).toHaveValue('relevance');
	await sort.selectOption({ label: 'Largest' });
	await expect.poll(() => sortOf(page)).toBe('-size');
	await expect.poll(() => sentSorts(mock).at(-1)).toBe('-size');
	await page.goBack();
	await expect.poll(() => sortOf(page)).toBeNull();
	await expect(sort).toHaveValue('relevance');
	// A pasted link's bare field is the descending option.
	await go('/search?q=lantern&sort=discovered');
	await expect(page.getByLabel('Sort')).toHaveValue('-discovered');
});

test('density switches the rows and stays in the URL', async ({ page, go, mock }) => {
	server(mock, () => body(hits(3)));
	await go('/search?q=lantern');
	await expect(row(page, 1)).toHaveAttribute('data-density', 'comfortable');
	await page.getByRole('radio', { name: 'Compact' }).click();
	await expect(page).toHaveURL(/density=compact/);
	await expect(row(page, 1)).toHaveAttribute('data-density', 'compact');
	await expect(page.getByRole('radio', { name: 'Compact' })).toBeFocused();
	await page.getByRole('radio', { name: 'Comfortable' }).click();
	await expect(page).not.toHaveURL(/density=/);
	await expect(row(page, 1)).toHaveAttribute('data-density', 'comfortable');
});

test('Save search saves the shown query and sort, and stays on the page', async ({
	page,
	go,
	mock
}) => {
	server(mock, () => body(hits(3)));
	mock.on('createSavedSearch', (request) => ({
		status: 201,
		body: {
			alert: true,
			alerts: 0,
			ast: { op: 'all' },
			created_at: 0,
			last_alert_at: null,
			paused: false,
			q: 'lantern',
			rss: false,
			sort: '-size',
			unread: 0,
			updated_at: 0,
			id: 9,
			name: (request.postDataJSON() as { name: string }).name
		}
	}));
	await go('/search?q=lantern&sort=-size');
	await page.getByRole('button', { name: 'Save search' }).click();
	const dialog = page.getByRole('dialog', { name: 'Save search' });
	await expect(dialog.getByLabel('Query')).toHaveValue('lantern');
	await dialog.getByLabel('Name').fill('Lanterns');
	await dialog.getByRole('button', { name: 'Save search' }).click();
	await expect(page.getByText('Saved “Lanterns”')).toBeVisible();
	await expect(dialog).toHaveCount(0);
	expect(mock.calls('createSavedSearch')[0]!.postDataJSON()).toMatchObject({
		name: 'Lanterns',
		q: 'lantern',
		sort: '-size'
	});
	await expect(page).toHaveURL(/\/search\?q=lantern&sort=-size/);
});

test('Load more asks for the next page once, then the end line takes focus', async ({
	page,
	go,
	mock,
	kbd
}) => {
	await page.setViewportSize({ width: 1366, height: 900 });
	server(mock, (sent) =>
		(sent as { cursor?: string | null }).cursor === 'p2'
			? body(hits(10, 50))
			: body(hits(50), { next_cursor: 'p2', total: 60 })
	);
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	const more = page.getByRole('button', { name: 'Load more torrents' });
	await kbd.tabTo(more);
	await kbd.press('Enter');
	const end = page.getByTestId('results-end');
	await expect(end).toBeFocused();
	await expect(more).toHaveCount(0);
	expect(resultsCalls(mock).filter((c) => c.cursor === 'p2')).toHaveLength(1);
});

for (const width of [1366, 390]) {
	test(`a pasted link does not shift as the toolbar and its count arrive at ${width} px`, async ({
		page,
		go,
		mock
	}) => {
		await page.setViewportSize({ width, height: 900 });
		server(mock, () => body(hits(3), { total: 3 }));
		await page.addInitScript(() => {
			const w = window as unknown as { __cls: number };
			w.__cls = 0;
			new PerformanceObserver((list) => {
				for (const e of list.getEntries() as unknown as {
					value: number;
					hadRecentInput: boolean;
				}[])
					if (!e.hadRecentInput) w.__cls += e.value;
			}).observe({ type: 'layout-shift', buffered: true });
		});
		await go('/search?q=lantern');
		await expect(count(page)).toHaveText('3 results');
		await expect(row(page, 1)).toBeVisible();
		const cls = await page.evaluate(() => (window as unknown as { __cls: number }).__cls);
		expect(cls).toBeLessThan(0.01);
	});
}

for (const mode of ['light', 'dark'] as const) {
	test.describe(`axe, Riverstone ${mode}`, () => {
		test.use({ themeId: `riverstone-${mode}`, colorMode: mode });
		for (const width of [1366, 390]) {
			test(`the toolbar, Load more and the end line at ${width} px`, async ({ page, go, mock }) => {
				await page.setViewportSize({ width, height: 900 });
				server(mock, (sent) =>
					JSON.stringify(sent.ast).includes('more')
						? body(hits(50), { next_cursor: 'p2', total: 60 })
						: body(hits(3))
				);
				await go('/search?q=lantern');
				await expect(page.getByTestId('results-end')).toBeVisible();
				await expectNoA11yViolations(page);
				await go('/search?q=more&density=compact');
				await expect(page.getByRole('button', { name: 'Load more torrents' })).toBeVisible();
				await expectNoA11yViolations(page);
			});
		}
	});
}
