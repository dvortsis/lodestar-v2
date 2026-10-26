import { discoverResponse, showAdultSettings } from '../src/lib/features/discover/testing';
import { response as facets } from '../src/lib/features/search/facets/fixtures';
import { body, expect, hits, resultsCalls, row, server } from './search-results.fixtures';
import { test } from './support/test';

test('Discover All opens immediate browsing with facets, sort and history-driven reset', async ({
	page,
	go,
	mock
}) => {
	await page.setViewportSize({ width: 1366, height: 800 });
	mock.on('getSettings', showAdultSettings(false));
	mock.on('getDiscover', discoverResponse());
	mock.on('facetsPost', facets());
	server(mock, () => body(hits(3)));
	await go('/discover?density=compact');
	await expect(page.getByTestId('discover-see-all')).toHaveAttribute('href', /browse=1/);
	await page.getByTestId('discover-see-all').click();
	await expect(row(page, 1)).toBeVisible();
	expect(resultsCalls(mock)[0].ast).toEqual({ op: 'all' });
	await expect.poll(() => mock.calls('facetsPost').length).toBeGreaterThan(0);
	expect(mock.calls('facetsPost')[0].postDataJSON().ast).toEqual({ op: 'all' });
	const density = new URL(page.url()).searchParams.get('density');
	await page.getByRole('combobox', { name: 'Sort' }).selectOption('-size');
	await expect.poll(() => mock.calls('searchPost').at(-1)?.postDataJSON().sort).toBe('-size');
	await page.getByTestId('facet-type-movie').check();
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('type:movie');
	await expect
		.poll(() => resultsCalls(mock).at(-1)?.ast)
		.toEqual({ op: 'tag', key: 'type', any: ['movie'] });
	await expect(page).toHaveURL(/browse=1/);
	await page.getByRole('button', { name: 'Clear all', exact: true }).click();
	await expect(page).not.toHaveURL(/browse=/);
	await expect(row(page, 1)).toHaveCount(0);
	expect(new URL(page.url()).searchParams.get('density')).toBe(density);
	await page.goBack();
	await expect(row(page, 1)).toBeVisible();
	await expect(page).toHaveURL(/browse=1/);
	await page.goForward();
	await expect(row(page, 1)).toHaveCount(0);
});

test('browse paging uses the same AST and explicit queries take precedence', async ({
	page,
	go,
	mock
}) => {
	server(mock, (sent) =>
		sent.cursor
			? body(hits(1, 50), { total: 51 })
			: body(hits(50), { total: 51, next_cursor: 'browse-next' })
	);
	await go('/search?browse=1');
	await expect(row(page, 1)).toBeVisible();
	await page.locator('[data-results-more]').scrollIntoViewIfNeeded();
	// The intersection sentinel can page before its keyboard fallback is activated.
	await expect.poll(() => resultsCalls(mock).some((r) => r.cursor === 'browse-next')).toBe(true);
	expect(resultsCalls(mock).every((r) => r.ast.op === 'all')).toBe(true);
	await go('/search?browse=1&q=type:movie');
	await expect(row(page, 1)).toBeVisible();
	expect(resultsCalls(mock).at(-1)?.ast).toEqual({ op: 'tag', key: 'type', any: ['movie'] });
	const count = resultsCalls(mock).length;
	await page.getByTestId('query-editor').fill('"unclosed');
	await page.getByTestId('query-editor').press('Enter');
	await expect(page.locator('#search-query-message')).toContainText('unclosed quote');
	expect(resultsCalls(mock)).toHaveLength(count);
});

test('an invalid direct query with browse enabled sends no match-all request', async ({
	page,
	go,
	mock
}) => {
	server(mock, () => body(hits(3)));
	await go('/search?browse=1&q=%22unclosed');
	await expect(page.locator('#search-query-message')).toContainText('unclosed quote');
	expect(resultsCalls(mock)).toHaveLength(0);
	await expect(row(page, 1)).toHaveCount(0);
});
