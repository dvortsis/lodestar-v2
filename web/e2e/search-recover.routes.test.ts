// Zero-result recovery (S-search-6; §10.4 "No results"): each filter removable in one click through
// the AST, and Clear all filters. Per-clause counts wait for the server (decisions file).
import { expectNoA11yViolations } from './a11y';
import { body, expect, hits, server } from './search-results.fixtures';
import { test } from './support/test';

test('an empty result offers each filter and Clear all filters', async ({ page, go, mock }) => {
	server(mock, (sent) => (JSON.stringify(sent.ast).includes('"ext"') ? body([]) : body(hits(2))));
	await go('/search?q=lantern+ext%3Amkv');
	const list = page.getByTestId('results-none-clauses');
	await expect(list).toBeVisible();
	await expectNoA11yViolations(page);
	await list.getByRole('button').first().click();
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('lantern');
	await expect(page.getByTestId('results-none')).toHaveCount(0);

	await go('/search?q=lantern+ext%3Amkv');
	await page.getByRole('button', { name: 'Clear all filters' }).click();
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('lantern');
});
