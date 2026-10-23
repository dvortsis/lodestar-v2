// Insights overview (S-insights-1): the story, its states, the URL contract, the crawler controls
// and the drill-throughs, against the OpenAPI mock.
import { expectNoA11yViolations } from './a11y';
import {
	crawlerFixture,
	COMPARE_KPI,
	NOW,
	overviewFixture
} from '../src/lib/features/insights/overview/fixtures';
import { expect, test } from './support/test';
import { crawlerEvent, liveEvents } from './support/events-server';

type Mock = Parameters<Parameters<typeof test>[2]>[0]['mock'];

function serve(mock: Mock, over = {}, crawler = {}) {
	mock.on('getInsightsOverview', overviewFixture(over));
	mock.on('getCrawler', crawlerFixture(crawler));
}
const ranges = (mock: Mock) =>
	mock.calls('getInsightsOverview').map((r) => new URL(r.url()).searchParams.get('range'));

test.describe('the story', () => {
	test('renders every level in order with its numbers', async ({ page, mock, go }) => {
		serve(mock);
		await go('/insights');
		await expect(page.getByTestId('insights-kpi-searchable-value')).toHaveText('1.2M');
		await expect(page.getByTestId('insights-stage-fetch-value')).toHaveText('41.1K');
		await expect(page.getByTestId('insights-funnel-stored-value')).toHaveText('40,012');
		const h2 = await page.locator('main h2').allTextContents();
		expect(h2).toEqual([
			'Status',
			'Needs attention',
			'Where torrents are lost',
			'Movement',
			'What is arriving',
			'Serve',
			'Detail'
		]);
		// Every chart subtitle names its time base.
		await expect(page.getByTestId('insights-page')).toContainText('Event time, in UTC');
		await expect(page.getByTestId('insights-page')).toContainText('Discovery cohort');
		await expect(page.getByTestId('insights-tz')).toHaveText('Times in UTC');
	});

	test('annotations are listed, gaps are not zeros, an incomplete cohort is labelled', async ({
		page,
		mock,
		go
	}) => {
		serve(mock);
		await go('/insights');
		await expect(page.getByTestId('insights-annotation-restart')).toContainText('2.4.1');
		await expect(page.getByTestId('insights-annotation-paused')).toBeVisible();
		await page.getByTestId('insights-chart-new-toggle').click();
		const rows = page.getByTestId('insights-chart-new-table').locator('tbody tr');
		await expect(rows).toHaveCount(48);
		await expect(rows.nth(7).locator('td').nth(1)).toHaveText('—');
		await expect(
			page.locator('[data-testid^="insights-cohort-"][data-testid$="-tag"]')
		).toHaveCount(1);
	});

	test('drill-through links keep the range', async ({ page, mock, go }) => {
		serve(mock);
		await go('/insights?range=30d&compare=1');
		await expect(page.getByTestId('insights-funnel-fetched').locator('a')).toHaveAttribute(
			'href',
			'/insights/fetch?range=30d'
		);
		await expect(page.getByTestId('insights-loss-timed_out').locator('a')).toHaveAttribute(
			'href',
			'/insights/fetch?range=30d&reason=timed_out'
		);
		await expect(page.getByTestId('insights-detail-serve')).toHaveAttribute(
			'href',
			'/insights/serve?range=30d&compare=1'
		);
		const search = await page.getByTestId('insights-group-video').locator('a').getAttribute('href');
		expect(decodeURIComponent(search!)).toBe('/search?q=type:movie,tv,video discovered:<30d');
		await expect(page.getByTestId('insights-group-unclassified').locator('a')).toHaveCount(0);
		await expect(page.getByTestId('insights-trend-csv-none')).toHaveCount(0);
		await expect(page.getByTestId('insights-chart-new-csv')).toHaveAttribute(
			'href',
			'/api/v1/insights/export.csv?panel=trend&range=30d&compare=1'
		);
	});

	test('view as table toggles with aria-expanded', async ({ page, mock, go }) => {
		serve(mock);
		await go('/insights');
		const toggle = page.getByTestId('insights-funnel-panel-toggle');
		await expect(toggle).toHaveAttribute('aria-expanded', 'false');
		await toggle.click();
		await expect(toggle).toHaveAttribute('aria-expanded', 'true');
		await expect(page.getByTestId('insights-funnel-panel-table')).not.toHaveClass(/sr-only/);
	});

	test('compare adds the previous series and the deltas', async ({ page, mock, go }) => {
		const previous = overviewFixture().trend.map((p) => ({ ...p }));
		mock.on('getInsightsOverview', (request) =>
			new URL(request.url()).searchParams.get('compare') === '1'
				? overviewFixture({
						kpi: { ...overviewFixture().kpi, ...COMPARE_KPI },
						previous: {
							kpi: {} as never,
							range: overviewFixture().range,
							segments: overviewFixture().segments,
							serve: overviewFixture().serve,
							stages: { discover: 1, fetch: 1, serve: null, understand: 1 },
							trend: previous
						}
					})
				: overviewFixture()
		);
		mock.on('getCrawler', crawlerFixture());
		await go('/insights');
		await expect(page.getByTestId('insights-kpi-searchable-delta')).toHaveCount(0);
		await page.getByText('Compare to previous period').click();
		await expect(page).toHaveURL(/compare=1/);
		await expect(page.getByTestId('insights-kpi-searchable-delta')).toContainText('+56.8K');
		await page.getByTestId('insights-chart-new-toggle').click();
		await expect(page.getByTestId('insights-chart-new-table').locator('th')).toHaveText([
			'Time',
			'Newly searchable',
			'Previous period'
		]);
	});
});

test.describe('states', () => {
	test('loading shows placeholders, then the page', async ({ page, mock, go }) => {
		let release!: () => void;
		const gate = new Promise<void>((r) => (release = r));
		// The mock awaits a handler; its type only allows sync ones.
		mock.on('getInsightsOverview', (async () => {
			await gate;
			return overviewFixture();
		}) as never);
		mock.on('getCrawler', crawlerFixture());
		await go('/insights');
		await expect(page.getByTestId('insights-loading')).toBeVisible();
		await expect(page.getByTestId('insights-bar')).toBeVisible();
		release();
		await expect(page.getByTestId('insights-kpis')).toBeVisible();
		await expect(page.getByTestId('insights-loading')).toHaveCount(0);
	});

	test('an error keeps the bar usable and Retry loads the page', async ({ page, mock, go }) => {
		mock.error('getInsightsOverview', 500, 'internal', 'boom');
		mock.on('getCrawler', crawlerFixture());
		await go('/insights');
		await expect(page.getByRole('alert')).toContainText('Insights could not be loaded');
		await expect(page.getByTestId('insights-pause')).toBeEnabled();
		mock.on('getInsightsOverview', overviewFixture());
		await page.getByTestId('insights-retry').click();
		await expect(page.getByTestId('insights-kpis')).toBeVisible();
	});

	test('a stale overview shows a warning with the reason', async ({ page, mock, go }) => {
		serve(mock, { stale: { reason: 'writer_stalled', since: NOW - 600 } });
		await go('/insights');
		await expect(page.getByTestId('insights-stale')).toContainText('Counting has stalled');
	});

	test('paused shows the scene copy and a Resume that refetches', async ({ page, mock, go }) => {
		const paused = {
			kpi: { ...overviewFixture().kpi, health: { state: 'paused' as const, reason: 'paused' } }
		};
		serve(mock, paused, { paused: true, state: 'paused' });
		mock.on('resumeCrawler', crawlerFixture());
		await go('/insights');
		await expect(page.getByTestId('insights-paused')).toContainText('The crawler is paused');
		const before = mock.calls('getInsightsOverview').length;
		serve(mock, {}, { paused: false });
		await page.getByTestId('insights-paused-resume').click();
		await expect.poll(() => mock.calls('resumeCrawler').length).toBe(1);
		await expect.poll(() => mock.calls('getInsightsOverview').length).toBeGreaterThan(before);
		await expect(page.getByTestId('insights-paused')).toHaveCount(0);
		await expect(page.getByTestId('insights-announce')).toHaveText('Crawler resumed');
	});

	test('starting up: 24 h has no final cohort, all has none either', async ({ page, mock, go }) => {
		const none = overviewFixture();
		serve(mock, { funnel_total: { ...none.funnel_total, final_cohorts: 0 } });
		await liveEvents(page, crawlerEvent({ harvested: 1234, fetched: 56, stored: 7 }));
		await go('/insights?range=24h');
		await expect(page.getByTestId('insights-starting')).toContainText(
			'Your first discoveries will appear here within minutes'
		);
		await expect.poll(() => ranges(mock)).toEqual(['24h', 'all']);
	});

	test('starting up is not shown when all time has a final cohort', async ({ page, mock, go }) => {
		mock.on('getInsightsOverview', (request) => {
			const r = new URL(request.url()).searchParams.get('range');
			const base = overviewFixture();
			return r === 'all'
				? base
				: overviewFixture({ funnel_total: { ...base.funnel_total, final_cohorts: 0 } });
		});
		mock.on('getCrawler', crawlerFixture());
		await go('/insights?range=24h');
		await expect.poll(() => ranges(mock)).toEqual(['24h', 'all']);
		await expect(page.getByTestId('insights-kpis')).toBeVisible();
		await expect(page.getByTestId('insights-starting')).toHaveCount(0);
	});

	test('search off and missing fetch counts show a dash with a reason', async ({
		page,
		mock,
		go
	}) => {
		const base = overviewFixture();
		serve(mock, {
			stages: { ...base.stages, index: null, fetch: null },
			kpi: { ...base.kpi, searchable: null, searchable_delta: null }
		});
		await go('/insights');
		await expect(page.getByTestId('insights-stage-index')).toContainText('Search is off');
		await expect(page.getByTestId('insights-stage-fetch')).toContainText('Not measured yet');
		await expect(page.getByTestId('insights-kpi-searchable-value')).toHaveText('—');
	});

	test('a resync refetches the overview and the crawler status', async ({ page, mock, go }) => {
		serve(mock);
		const events = await liveEvents(page);
		await go('/insights');
		await expect(page.getByTestId('insights-kpis')).toBeVisible();
		const overview = mock.calls('getInsightsOverview').length;
		const crawler = mock.calls('getCrawler').length;
		await events.send('resync', {});
		await expect.poll(() => mock.calls('getInsightsOverview').length).toBe(overview + 1);
		await expect.poll(() => mock.calls('getCrawler').length).toBe(crawler + 1);
	});

	test('a lower level that cannot load shows an error with Retry', async ({ page, mock, go }) => {
		serve(mock);
		let block = true;
		await page.route('**/_app/immutable/**/*.js', async (route) => {
			const response = await route.fetch();
			const body = await response.text();
			if (block && body.includes('Funnel by discovery cohort')) return route.abort();
			return route.fulfill({ response, body });
		});
		await go('/insights');
		const error = page.getByTestId('insights-chunk-error');
		await expect(error).toBeVisible();
		await expect(page.getByTestId('insights-kpis')).toBeVisible();
		block = false;
		await error.getByRole('button', { name: 'Try again' }).click();
		await expect(page.getByTestId('insights-funnel')).toBeVisible();
	});

	test('a live insights event refetches in place', async ({ page, mock, go }) => {
		serve(mock);
		const events = await liveEvents(page);
		await go('/insights');
		await expect(page.getByTestId('insights-kpis')).toBeVisible();
		const before = mock.calls('getInsightsOverview').length;
		serve(mock, { kpi: { ...overviewFixture().kpi, new_searchable: 5 } });
		await events.send('insights', { stages: overviewFixture().stages, updated_at: NOW });
		await expect(page.getByTestId('insights-kpi-new-value')).toHaveText('5');
		expect(mock.calls('getInsightsOverview').length).toBe(before + 1);
		await expect(page.getByTestId('insights-loading')).toHaveCount(0);
	});
});

test.describe('the URL is the state', () => {
	test('a range change pushes a URL and makes one new request; Back restores it', async ({
		page,
		mock,
		go
	}) => {
		serve(mock);
		await go('/insights');
		await expect(page.getByTestId('insights-kpis')).toBeVisible();
		await page.getByRole('radio', { name: '30 d' }).click();
		await expect(page).toHaveURL(/range=30d/);
		await expect.poll(() => ranges(mock)).toEqual(['7d', '30d']);
		await expect(page.getByTestId('insights-range-chip')).toHaveText('Last 30 days');
		await page.goBack();
		await expect(page).not.toHaveURL(/range=/);
		await expect.poll(() => ranges(mock)).toEqual(['7d', '30d', '7d']);
		await page.goForward();
		await expect(page).toHaveURL(/range=30d/);
	});

	test('removing the chip returns to 7 d', async ({ page, mock, go }) => {
		serve(mock);
		await go('/insights?range=24h');
		await page.getByRole('button', { name: /Reset the range/ }).click();
		await expect(page).not.toHaveURL(/range=/);
	});

	test('invalid parameters fall back quietly', async ({ page, mock, go }) => {
		serve(mock);
		await go('/insights?range=bogus&compare=yes');
		await expect(page.getByTestId('insights-kpis')).toBeVisible();
		await expect(page).toHaveURL(/\/insights(\?density=\w+)?$/);
		await go('/insights?range=custom&from=9&to=1');
		await expect(page).toHaveURL(/\/insights(\?density=\w+)?$/);
	});

	test('compare is shown but unavailable for All', async ({ page, mock, go }) => {
		serve(mock);
		await go('/insights?range=all&compare=1');
		await expect(page.getByTestId('insights-compare')).toBeDisabled();
		await expect(page.getByTestId('insights-compare-why')).toHaveText('Not available for All');
		const query = new URL(mock.calls('getInsightsOverview')[0]!.url()).searchParams;
		expect(query.get('compare')).toBeNull();
	});

	test('a custom range converts UTC days to seconds and shows a server 400 on its field', async ({
		page,
		mock,
		go
	}) => {
		serve(mock);
		await go('/insights');
		await page.getByRole('radio', { name: 'Custom' }).click();
		await page.getByTestId('insights-custom-from').fill('2026-10-01');
		await page.getByTestId('insights-custom-to').fill('2026-10-07');
		mock.error('getInsightsOverview', 400, 'invalid_range', 'The range is too long.', {
			field: 'to'
		});
		await page.getByRole('button', { name: 'Apply range' }).click();
		await expect(page).toHaveURL(
			new RegExp(
				`range=custom&from=${Date.UTC(2026, 9, 1) / 1000}&to=${Date.UTC(2026, 9, 8) / 1000}`
			)
		);
		await expect(page.getByTestId('insights-custom-to-error')).toHaveText('The range is too long.');
		await expect(page.getByTestId('insights-custom-to')).toHaveAttribute('aria-invalid', 'true');
		// A bad range is not "Lodestar did not answer".
		await expect(page.getByTestId('insights-range-invalid')).toBeVisible();
		await expect(page.getByTestId('insights-retry')).toHaveCount(0);
	});
});

test.describe('crawler controls', () => {
	test('Pause posts, then refetches the crawler and the overview', async ({ page, mock, go }) => {
		serve(mock);
		mock.on('pauseCrawler', crawlerFixture({ paused: true, state: 'paused' }));
		await go('/insights');
		await expect(page.getByTestId('insights-pause')).toBeVisible();
		const crawlerBefore = mock.calls('getCrawler').length;
		const overviewBefore = mock.calls('getInsightsOverview').length;
		mock.on('getCrawler', crawlerFixture({ paused: true, state: 'paused' }));
		await page.getByTestId('insights-pause').click();
		await expect.poll(() => mock.calls('pauseCrawler').length).toBe(1);
		await expect(page.getByTestId('insights-resume')).toBeVisible();
		expect(mock.calls('getCrawler').length).toBeGreaterThan(crawlerBefore);
		expect(mock.calls('getInsightsOverview').length).toBeGreaterThan(overviewBefore);
		await expect(page.getByTestId('insights-announce')).toHaveText('Crawler paused');
	});

	test('a failed pause is shown next to the control', async ({ page, mock, go }) => {
		serve(mock);
		mock.error('pauseCrawler', 500, 'internal', 'no');
		await go('/insights');
		await page.getByTestId('insights-pause').click();
		await expect(page.getByTestId('insights-crawler-error')).toContainText(
			'Could not pause the crawler'
		);
		await expect(page.getByTestId('insights-pause')).toBeVisible();
	});

	test('intensity PATCHes the level; an env-set level is locked', async ({ page, mock, go }) => {
		serve(mock);
		mock.on('patchCrawler', crawlerFixture({ intensity: 'low' }));
		await go('/insights');
		await page.getByRole('radio', { name: 'Low' }).click();
		await expect.poll(() => mock.calls('patchCrawler').length).toBe(1);
		expect(mock.calls('patchCrawler')[0]!.postDataJSON()).toEqual({ intensity: 'low' });
	});

	test('a failed intensity change shows what the server has', async ({ page, mock, go }) => {
		serve(mock);
		mock.error('patchCrawler', 500, 'internal', 'no');
		await go('/insights');
		await page.getByRole('radio', { name: 'Aggressive' }).click();
		await expect(page.getByTestId('insights-crawler-error')).toContainText('change the intensity');
		await expect(page.getByRole('radio', { name: 'Balanced' })).toBeChecked();
	});

	test('intensity set by an environment variable has no control', async ({ page, mock, go }) => {
		serve(mock, {}, { intensity_env: 'LODESTAR_INTENSITY' });
		await go('/insights');
		await expect(page.getByTestId('insights-intensity-locked')).toContainText(
			'Set by LODESTAR_INTENSITY'
		);
		await expect(page.getByRole('radio', { name: 'Low' })).toHaveCount(0);
	});
});

test.describe('layout', () => {
	test.use({ viewport: { width: 390, height: 800 } });
	test('narrow: exceptions and the funnel come before the trend, no horizontal scroll', async ({
		page,
		mock,
		go
	}) => {
		serve(mock);
		await go('/insights');
		await expect(page.getByTestId('insights-kpis')).toBeVisible();
		const top = (id: string) => page.locator(id).evaluate((el) => el.getBoundingClientRect().top);
		const [exc, funnel, move, seg, srv] = await Promise.all(
			[
				'#insights-exceptions',
				'#insights-funnel',
				'#insights-movement',
				'#insights-segments',
				'#insights-serve'
			].map(top)
		);
		expect(exc!).toBeLessThan(funnel!);
		expect(funnel!).toBeLessThan(move!);
		expect(move!).toBeLessThan(seg!);
		expect(seg!).toBeLessThan(srv!);
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(
			true
		);
	});
});

for (const mode of ['light', 'dark'] as const) {
	for (const size of [
		{ width: 1366, height: 900 },
		{ width: 390, height: 800 }
	]) {
		test.describe(`axe, riverstone ${mode}, ${size.width}`, () => {
			test.use({ themeId: `riverstone-${mode}`, colorMode: mode, viewport: size });
			test('has no violations in the story or in its states', async ({ page, mock, go }) => {
				serve(mock);
				await go('/insights');
				await expect(page.getByTestId('insights-funnel')).toBeVisible();
				await expectNoA11yViolations(page);
				await page.getByTestId('insights-funnel-panel-toggle').click();
				await page.getByRole('radio', { name: 'Custom' }).click();
				await expectNoA11yViolations(page);
			});
			test('has no violations when paused, stale or failed', async ({ page, mock, go }) => {
				const base = overviewFixture();
				serve(
					mock,
					{
						kpi: { ...base.kpi, health: { state: 'paused', reason: 'paused' } },
						stale: { reason: 'paused', since: NOW - 600 }
					},
					{ paused: true, state: 'paused' }
				);
				await go('/insights');
				await expect(page.getByTestId('insights-paused')).toBeVisible();
				await expectNoA11yViolations(page);
				mock.error('getInsightsOverview', 500, 'internal', 'boom');
				await go('/insights?range=24h');
				await expect(page.getByTestId('insights-retry')).toBeVisible();
				await expectNoA11yViolations(page);
			});
		});
	}
}
