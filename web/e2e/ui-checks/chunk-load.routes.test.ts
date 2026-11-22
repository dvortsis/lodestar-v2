// UI check (review 02 §3e, 03 "A lazy chunk fails to load"): most screens load on first use, so
// one failed request for a lazy chunk must leave a page that says so and recovers, not a blank one.
// Each case blocks the chunk that holds a screen (found by a string only that screen's code
// has, so no hashed file name is pinned), shows the page's own error, then lets the chunk through
// and follows its Retry or Reload. Waits are conditions with a 10 s deadline; no fixed sleeps.
// Ported from the review's flows; named `.routes.test.ts` so the `core` project picks it up.
import type { Page } from '@playwright/test';
import { body, hits, indexState, row, rowOf, server } from '../search-results.fixtures';
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

	// The results, facets and toolbar load as one lazy set. A failed set leaves an alert where the
	// results go, with no uncaught rejection. Retry reloads the page (Chrome for Testing 153 keeps a
	// failed dynamic import for the module graph, so a second `import()` can't recover), and the
	// reload carries the URL's query, a new one typed in the failed state included.
	async function failSearch(page: Page, go: (path: string) => Promise<unknown>, mock: OpenApiMock) {
		const errors: string[] = [];
		page.on('pageerror', (error) => errors.push(error.message));
		server(mock, () => body(hits(2)));
		mock.on('searchIndexState', indexState(5_000_000));
		const gate = await blockChunk(page, 'results-announce');
		await go('/search?q=lantern');
		const failed = page.locator('#results [role="alert"]');
		await expect(failed).toHaveText("Results couldn't load.Retry");
		expect(gate.hit).toBeGreaterThan(0);
		await expect(page.getByRole('grid', { name: 'Matching torrents' })).toHaveCount(0);
		return { errors, failed, gate };
	}

	/** Whether focus is inside the results region: never lost to the page body. */
	const focusInResults = (page: Page) =>
		page.evaluate(() => !!document.activeElement?.closest('#results'));

	test('Search results: an alert where the results go, and Retry brings them back', async ({
		page,
		go,
		mock
	}) => {
		const { errors, failed, gate } = await failSearch(page, go, mock as OpenApiMock);
		gate.blocked = false;
		// Keyboard only: Retry is within a few Tab stops of the query bar (the bar's Syntax button
		// comes first), and Enter runs it.
		const retry = failed.getByRole('button', { name: 'Retry' });
		await page.getByRole('combobox', { name: 'Search torrents' }).focus();
		let reached = false;
		for (let i = 0; i < 4 && !reached; i++) {
			await page.keyboard.press('Tab');
			reached = await retry.evaluate((el) => el === document.activeElement);
		}
		expect(reached).toBe(true);
		await page.keyboard.press('Enter');
		await expect(page.getByRole('grid', { name: 'Matching torrents' })).toBeVisible();
		await expect(failed).toHaveCount(0);
		await expect.poll(() => focusInResults(page)).toBe(true);
		// The marker that sends focus there never reaches the URL, and is used up.
		expect(new URL(page.url()).hash).toBe('');
		expect(await page.evaluate(() => window.name)).toBe('');
		expect(errors).toEqual([]);
	});

	test('Search results: a new query typed in the failed state, then Retry, shows its results', async ({
		page,
		go,
		mock
	}) => {
		const { errors, failed, gate } = await failSearch(page, go, mock as OpenApiMock);
		// The alert is announced once: a new query's attempt that fails again keeps the same node.
		await failed.evaluate((el) => el.setAttribute('data-first', ''));
		const box = page.getByRole('combobox', { name: 'Search torrents' });
		await box.click();
		await page.keyboard.press('ControlOrMeta+a');
		await page.keyboard.type('harbor');
		await expect(page).toHaveURL(/[?&]q=harbor/);
		await expect(page.locator('#results [role="alert"][data-first]')).toHaveCount(1);
		await expect(failed).toHaveCount(1);
		gate.blocked = false;
		await failed.getByRole('button', { name: 'Retry' }).click();
		await expect(page.getByRole('grid', { name: 'Matching torrents' })).toBeVisible();
		await expect(page).toHaveURL(/[?&]q=harbor/);
		await expect(box).toHaveText('harbor');
		await expect(failed).toHaveCount(0);
		await expect.poll(() => focusInResults(page)).toBe(true);
		expect(errors).toEqual([]);
	});

	test('Search results: Retry while the chunk still fails shows the alert again, with no error', async ({
		page,
		go,
		mock
	}) => {
		const { errors, failed, gate } = await failSearch(page, go, mock as OpenApiMock);
		const before = gate.hit;
		await failed.getByRole('button', { name: 'Retry' }).click();
		// The reload asked for the chunk again, and it failed again.
		await expect.poll(() => gate.hit).toBeGreaterThan(before);
		await expect(failed).toHaveText("Results couldn't load.Retry");
		await expect(page.getByRole('grid', { name: 'Matching torrents' })).toHaveCount(0);
		// The failed load used up the one-shot marker (the next case shows a later load leaves focus alone).
		expect(await page.evaluate(() => window.name)).toBe('');
		expect(errors).toEqual([]);
	});
	test('Search results: after Retry on a bare /search, typing in the bar keeps focus and every key', async ({
		page,
		go,
		mock
	}) => {
		const errors: string[] = [];
		page.on('pageerror', (error) => errors.push(error.message));
		server(mock, () => body(hits(2)));
		mock.on('searchIndexState', indexState(5_000_000));
		const gate = await blockChunk(page, 'results-announce');
		await go('/search');
		const box = page.getByRole('combobox', { name: 'Search torrents' });
		// With no query, the results load on the first interaction: clicking the bar starts it.
		await box.click();
		const failed = page.locator('#results [role="alert"]');
		await expect(failed).toBeVisible();
		gate.blocked = false;
		await failed.getByRole('button', { name: 'Retry' }).click();
		// After the reload the load starts with the click into the bar, so it settles while typing.
		await expect(box).toBeVisible();
		await box.click();
		await page.keyboard.type('abcdef', { delay: 50 });
		await expect(page.getByRole('grid', { name: 'Matching torrents' })).toBeVisible();
		await expect(box).toBeFocused();
		await expect(box).toHaveText('abcdef');
		expect(await page.evaluate(() => window.name)).toBe('');
		expect(errors).toEqual([]);
	});

	test('Search results: Retry pressed within 100 ms of typing reloads at the typed query', async ({
		page,
		go,
		mock
	}) => {
		const { errors, failed, gate } = await failSearch(page, go, mock as OpenApiMock);
		gate.blocked = false;
		const box = page.getByRole('combobox', { name: 'Search torrents' });
		await box.click();
		await page.keyboard.press('ControlOrMeta+a');
		await page.keyboard.type('harbor');
		// Inside the bar's 200 ms debounce: the URL still holds the old query.
		expect(new URL(page.url()).searchParams.get('q')).toBe('lantern');
		await failed.getByRole('button', { name: 'Retry' }).click();
		await expect(page.getByRole('grid', { name: 'Matching torrents' })).toBeVisible();
		await expect(page).toHaveURL(/[?&]q=harbor(&|$)/);
		await expect(box).toHaveText('harbor');
		expect(errors).toEqual([]);
	});

	test('Search results: Retry while /parse hangs reloads at the raw typed text within about 1 s', async ({
		page,
		go,
		mock
	}) => {
		const { errors, failed, gate } = await failSearch(page, go, mock as OpenApiMock);
		gate.blocked = false;
		// Parse requests hang until the reload; the reloaded page's are answered as usual. The
		// stand-in's canonical form collapses spaces, so the raw text (two spaces) is told apart.
		let slow = true;
		await page.route('**/api/v1/parse?**', (route) => (slow ? undefined : route.fallback()));
		page.once('load', () => (slow = false));
		const box = page.getByRole('combobox', { name: 'Search torrents' });
		await box.click();
		await page.keyboard.press('ControlOrMeta+a');
		await page.keyboard.type('harbor  slow');
		const start = Date.now();
		await failed.getByRole('button', { name: 'Retry' }).click();
		await expect(page).toHaveURL(/[?&]q=harbor\+\+slow(&|$)/);
		// The bound is 1 s; the rest is the click and the URL write, with room for a slow runner.
		expect(Date.now() - start).toBeLessThan(4_000);
		await expect(page.getByRole('grid', { name: 'Matching torrents' })).toBeVisible();
		expect(errors).toEqual([]);
	});

	test('Search results: after Retry, a load the 30 s timer starts never moves focus', async ({
		page,
		go,
		mock
	}) => {
		const errors: string[] = [];
		page.on('pageerror', (error) => errors.push(error.message));
		server(mock, () => body(hits(2)));
		mock.on('searchIndexState', indexState(5_000_000));
		await page.clock.install();
		const gate = await blockChunk(page, 'results-announce');
		await go('/search');
		await page.getByRole('combobox', { name: 'Search torrents' }).click();
		const failed = page.locator('#results [role="alert"]');
		await expect(failed).toBeVisible();
		// Retry while the chunk still fails: the reload's own load, started at mount, uses the marker
		// up and fails; nothing is focused.
		const before = gate.hit;
		await failed.getByRole('button', { name: 'Retry' }).click();
		await expect.poll(() => gate.hit).toBeGreaterThan(before);
		await expect(failed).toBeVisible();
		expect(await page.evaluate(() => window.name)).toBe('');
		// The 30 s fallback then starts another load (the shell's chrome arrives with the same timer).
		// Whether that load fails again (Chromium 153 keeps the failure) or succeeds, focus stays put.
		gate.blocked = false;
		await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
		await page.clock.runFor(31_000);
		await expect(page.getByTestId('palette-button')).toBeAttached();
		await expect(
			failed.or(page.getByRole('grid', { name: 'Matching torrents' })).first()
		).toBeVisible();
		await page.clock.runFor(1_000); // the success path's focus hand-off is a timeout
		expect(await page.evaluate(() => document.activeElement === document.body)).toBe(true);
		expect(errors).toEqual([]);
	});

	/** Opens result 1's drawer from Search while the drawer's chunk is blocked. */
	async function failDrawer(page: Page, go: (path: string) => Promise<unknown>, mock: OpenApiMock) {
		const errors: string[] = [];
		page.on('pageerror', (error) => errors.push(error.message));
		server(mock, () => body(hits(2)));
		mock.on('searchIndexState', indexState(5_000_000));
		// DetailHost's chunk is the only one that preloads DetailHostBody's stylesheet.
		const gate = await blockChunk(page, 'assets/DetailHostBody.');
		await go('/search?q=lantern');
		await expect(row(page, 1)).toBeVisible();
		await rowOf(page, 1).focus();
		await page.keyboard.press('Enter');
		const failed = page.locator('#search-drawer [role="alert"]');
		await expect(failed).toContainText("Details couldn't load.");
		expect(gate.hit).toBeGreaterThan(0);
		// The alert is announced, not focused: the row keeps focus.
		await expect(rowOf(page, 1)).toBeFocused();
		return { errors, failed, gate };
	}

	test('the detail drawer: a failed chunk shows an alert, and Retry opens the torrent', async ({
		page,
		go,
		mock
	}) => {
		const { errors, failed, gate } = await failDrawer(page, go, mock as OpenApiMock);
		const url = new URL(page.url());
		expect(url.pathname).toMatch(/^\/t\//);
		gate.blocked = false;
		// The full page finds its hit by hash, without Search's seed.
		(mock as OpenApiMock).on('searchGet', body(hits(1)));
		await failed.getByRole('button', { name: 'Retry' }).click();
		// A reload drops the drawer's page.state, so the torrent opens as its full page.
		await expect(page.getByTestId('detail-view')).toHaveAttribute('data-mode', 'page');
		expect(new URL(page.url()).pathname).toBe(url.pathname);
		await expect(page.locator('#search-drawer')).toHaveCount(0);
		expect(errors).toEqual([]);
	});

	test('the detail drawer: Close leaves the failed drawer as closing it does', async ({
		page,
		go,
		mock
	}) => {
		const { errors, failed } = await failDrawer(page, go, mock as OpenApiMock);
		await failed.getByRole('button', { name: 'Close' }).click();
		await expect(failed).toHaveCount(0);
		await expect(page).toHaveURL(/\/search\?q=lantern(&|$)/);
		await expect(page.getByRole('grid', { name: 'Matching torrents' })).toBeVisible();
		// The drawer's entry is gone: Back now leaves Search's own entry, never reopens the drawer.
		expect(await page.evaluate(() => 'detail' in (history.state?.['sveltekit:states'] ?? {}))).toBe(
			false
		);
		expect(errors).toEqual([]);
	});

	test('the detail drawer: Back leaves the failed drawer, and the alert goes with it', async ({
		page,
		go,
		mock
	}) => {
		const { errors, failed } = await failDrawer(page, go, mock as OpenApiMock);
		await page.goBack();
		await expect(failed).toHaveCount(0);
		await expect(page).toHaveURL(/\/search\?q=lantern(&|$)/);
		await expect(page.getByRole('grid', { name: 'Matching torrents' })).toBeVisible();
		expect(errors).toEqual([]);
	});
});
