// §10.17 brand gate: a visual snapshot of every illustrated full-page state in light and dark at
// each breakpoint. Runs in the `bp<w>-riverstone-<mode>-<density>` matrix projects (Compact repeats
// the same pixels, so it only checks the page renders and passes axe). Every state is reached
// through the API mock's answers; the shell tests cover the illustration's markup.
import type { Page } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import { crawlerFixture, overviewFixture } from '../src/lib/features/insights/overview/fixtures';
import {
	response as discover,
	newest,
	showAdultSettings
} from '../src/lib/features/discover/testing';
import { indexState } from './search-results.fixtures';
import { expectPageSnapshot } from './snapshot';
import { crawlerEvent, engage, liveEvents } from './support/events-server';
import { interact } from './support/interact';
import { expect as baseExpect, test } from './support/test';

const expect = baseExpect.configure({ timeout: 10_000 });

/** Waits for the scene inside `scope` to decode, then axe, then a Comfortable-only baseline. */
async function settle(page: Page, density: string, scope: string, name: string) {
	// The scene is lazy and decoded async: snapshot only once it has actually loaded.
	const img = page.locator(`${scope} img`);
	await expect(img).toBeVisible();
	await expect
		.poll(() => img.evaluate((el: HTMLImageElement) => el.complete && el.naturalWidth > 0))
		.toBe(true);
	await expectNoA11yViolations(page);
	if (density === 'comfortable') await expectPageSnapshot(page, name);
}

test('illustrated state: not found', async ({ page, go, density }) => {
	await go('/definitely-not-a-page');
	await expect(page.getByRole('heading', { level: 1, name: 'Page not found' })).toBeVisible();
	await settle(page, density, '[data-testid="state-screen"]', 'state-not-found');
});

test('illustrated state: connection error', async ({ page, go, density }) => {
	// Nothing answers: the stream, the probe and the REST calls (see shell.routes.test.ts).
	await page.route('**/api/v1/**', (route) => route.abort('connectionrefused'));
	await go('/search');
	await engage(page);
	await expect(page.getByRole('heading', { level: 1, name: 'Can’t reach Lodestar' })).toBeVisible();
	await settle(page, density, '[data-testid="state-screen"]', 'state-connection-error');
});

test('illustrated state: search, index still growing', async ({ page, go, mock, density }) => {
	mock.on('parseQuery', { ast: { op: 'all' }, canonical: '' });
	mock.on('listSavedSearches', []);
	mock.on('searchIndexState', indexState(120, { cold_start: true, growing: true }));
	await go('/search');
	await interact(page);
	await expect(page.getByTestId('results-cold-page')).toBeVisible();
	await settle(page, density, '[data-testid="results-cold-page"]', 'state-search-cold');
});

test('illustrated state: insights, crawler paused', async ({ page, go, mock, density }) => {
	const base = overviewFixture();
	mock.on(
		'getInsightsOverview',
		overviewFixture({ kpi: { ...base.kpi, health: { state: 'paused', reason: 'paused' } } })
	);
	mock.on('getCrawler', crawlerFixture({ paused: true, state: 'paused' }));
	await go('/insights');
	await expect(page.getByTestId('insights-paused')).toBeVisible();
	await settle(page, density, '[data-testid="insights-paused"]', 'state-insights-paused');
});

test('illustrated state: insights, starting up', async ({ page, go, mock, density }) => {
	const none = overviewFixture();
	mock.on(
		'getInsightsOverview',
		overviewFixture({ funnel_total: { ...none.funnel_total, final_cohorts: 0 } })
	);
	mock.on('getCrawler', crawlerFixture());
	await liveEvents(page, crawlerEvent({ harvested: 1234, fetched: 56, stored: 7 }));
	await go('/insights');
	await expect(page.getByTestId('insights-starting')).toBeVisible();
	await expect(page.getByTestId('insights-starting-harvested')).toHaveText('1,234');
	await settle(page, density, '[data-testid="insights-starting"]', 'state-insights-starting');
});

test('illustrated state: empty Discover', async ({ page, go, mock, density }) => {
	mock.on('getSettings', showAdultSettings(false));
	mock.on('getDiscover', discover({ newest: newest({ hits: [], total: 0 }) }));
	await go('/discover');
	await expect(page.getByRole('heading', { name: 'Nothing discovered yet' })).toBeVisible();
	await settle(page, density, 'main', 'state-discover-empty');
});

test('illustrated state: empty Alerts', async ({ page, go, mock, density }) => {
	mock.on('listSavedSearches', []);
	await go('/alerts');
	await expect(page.getByRole('heading', { name: 'Alerts', level: 1 })).toBeVisible();
	await settle(page, density, 'main', 'state-alerts-empty');
});

test('illustrated state: Compare with nothing chosen', async ({ page, go, density }) => {
	await go('/compare');
	await expect(page.getByTestId('compare-choose')).toBeVisible();
	await settle(page, density, '[data-testid="compare-choose"]', 'state-compare-empty');
});
