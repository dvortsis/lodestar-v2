// UI check (review 02 §3e, 03 "A lazy chunk fails to load"): most screens load on first use, so
// one failed request for a lazy chunk must leave a page that says so and recovers, not a blank one.
// Each case blocks the chunk that holds a screen (found by a string only that screen's code
// has, so no hashed file name is pinned), shows the page's own error, then lets the chunk through
// and follows its Retry or Reload. Waits are conditions with a 10 s deadline; no fixed sleeps.
// Ported from the review's flows; named `.routes.test.ts` so the `core` project picks it up.
import type { Page } from '@playwright/test';
import { body, hits, indexState, server } from '../search-results.fixtures';
import { crawlerFixture, overviewFixture } from '../../src/lib/features/insights/overview/fixtures';
import {
	SettingsBackend,
	type MockLike
} from '../../src/lib/features/settings/form/testing/e2e-backend';
import type { OpenApiMock } from '../mock/openapi-mock';
import { SETTINGS_INDEX, mockSettings } from '../support/settings-fixture';
import { expect as baseExpect, test } from '../support/test';

const expect = baseExpect.configure({ timeout: 10_000 });

/**
 * Aborts the app's JS chunks whose source contains `marker` while `gate.blocked` is true, and
 * lets everything else through. The marker is a test id or message key only that chunk carries.
 */
async function blockChunk(page: Page, marker: string) {
	const gate = { blocked: true, hit: 0 };
	await page.route('**/_app/immutable/**/*.js', async (route) => {
		try {
			const response = await route.fetch();
			const text = await response.text();
			if (gate.blocked && text.includes(marker)) {
				gate.hit++;
				await route.abort('connectionrefused');
			} else {
				await route.fulfill({ response, body: text });
			}
		} catch (error) {
			// The page closed, or the response was disposed with it, while the request was in
			// flight (the end of the test, or a Reload that replaced the page); there is nothing
			// left to answer. Anything else is a real failure.
			if (!page.isClosed() && !/closed|disposed/i.test(String(error))) throw error;
		}
	});
	return gate;
}

test.describe('a lazy chunk fails to load', () => {
	test('Insights overview: the page says so, and Retry brings it back', async ({
		page,
		go,
		mock
	}) => {
		mock.on('getInsightsOverview', overviewFixture());
		mock.on('getCrawler', crawlerFixture());
		const gate = await blockChunk(page, 'data-testid="insights-page"');
		await go('/insights');
		const failed = page.getByTestId('insights-load-error');
		await expect(failed).toBeVisible();
		await expect(failed).toHaveAttribute('role', 'alert');
		expect(gate.hit).toBeGreaterThan(0);
		// The shell around it still works: the top bar's navigation is there.
		await expect(page.getByRole('link', { name: 'Search', exact: true }).first()).toBeVisible();
		gate.blocked = false;
		await failed.getByRole('button').click();
		await expect(page.getByTestId('insights-page')).toBeVisible();
		await expect(failed).toHaveCount(0);
	});

	test('an Insights stage page: the same, and Retry brings it back', async ({ page, go }) => {
		const gate = await blockChunk(page, 'data-testid="insights-stage-page"');
		await go('/insights/discover?range=30d');
		const failed = page.getByTestId('insights-load-error');
		await expect(failed).toBeVisible();
		await expect(failed).toHaveAttribute('role', 'alert');
		expect(gate.hit).toBeGreaterThan(0);
		gate.blocked = false;
		await failed.getByRole('button').click();
		await expect(page.getByTestId('insights-stage-page')).toBeVisible();
	});

	test('the Settings overview: a banner with Reload, and Reload brings it back', async ({
		page,
		go,
		mock
	}) => {
		mockSettings(mock);
		const gate = await blockChunk(page, 'data-testid="attention-empty"');
		await go('/settings');
		await expect(page.getByText("The overview couldn't be loaded.")).toBeVisible();
		// The heading and the section list are not lazy, so the page is still a page.
		await expect(page.getByRole('heading', { level: 1, name: 'Overview' })).toBeVisible();
		expect(gate.hit).toBeGreaterThan(0);
		gate.blocked = false;
		await page.getByRole('button', { name: 'Reload' }).click();
		await expect(page.getByText("The overview couldn't be loaded.")).toHaveCount(0);
		await expect(
			page.getByTestId('attention-list').or(page.getByTestId('attention-empty'))
		).toBeVisible();
	});

	test('a Settings screen body: a banner with Reload, and Reload brings it back', async ({
		page,
		go,
		mock
	}) => {
		new SettingsBackend(SETTINGS_INDEX, []).attach(mock as unknown as MockLike);
		const on = (mock as OpenApiMock).on.bind(mock) as (id: string, f: unknown) => unknown;
		on('listCompanions', { companions: [] });
		const gate = await blockChunk(page, 'id="companions-heading"');
		await go('/settings/companions');
		await expect(
			page.getByText(
				"Part of this page couldn't load. Check your connection, then reload the page."
			)
		).toBeVisible();
		await expect(page.getByRole('button', { name: 'Reload' })).toBeVisible();
		expect(gate.hit).toBeGreaterThan(0);
		gate.blocked = false;
		await page.getByRole('button', { name: 'Reload' }).click();
		await expect(page.locator('#companions-heading')).toBeVisible();
		await expect(
			page.getByText(
				"Part of this page couldn't load. Check your connection, then reload the page."
			)
		).toHaveCount(0);
	});

	// EXPECTED GAP, not the wanted behaviour: when the Search results chunk fails, the page shows
	// only the query bar, no results and no message, and raises an uncaught rejection, because
	// `loadResults()` in src/routes/search/+page.svelte has no `.catch`. The follow-up slice
	// X-search-chunk-retry adds a retry state; it must flip this case to assert that state
	// (a role=alert message with a Retry that brings the results back) and drop the page error.
	test('Search results (expected gap, X-search-chunk-retry): blank query-bar page and an uncaught error', async ({
		page,
		go,
		mock
	}) => {
		const errors: string[] = [];
		page.on('pageerror', (error) => errors.push(error.message));
		server(mock, () => body(hits(2)));
		mock.on('searchIndexState', indexState(5_000_000));
		const gate = await blockChunk(page, 'results-announce');
		await go('/search?q=lantern');
		await expect(page.getByRole('combobox', { name: 'Search torrents' })).toBeVisible();
		await expect.poll(() => gate.hit).toBeGreaterThan(0);
		await expect(page.getByRole('grid', { name: 'Matching torrents' })).toHaveCount(0);
		await expect(page.getByTestId('results-announce')).toHaveCount(0);
		await expect
			.poll(() => errors.filter((e) => e.includes('Failed to fetch dynamically imported module')))
			.toHaveLength(1);
	});
});
