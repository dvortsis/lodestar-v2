// Insights stage pages (S-insights-2): each page renders against the OpenAPI mock, keeps the range
// and Compare in its links, says "not available yet" for a missing series, and passes axe.
import { expectNoA11yViolations } from './a11y';
import {
	discoverFixture,
	fetchFixture,
	indexFixture,
	serveFixture,
	understandFixture
} from '../src/lib/features/insights/stages/fixtures';
import { crawlerFixture, overviewFixture } from '../src/lib/features/insights/overview/fixtures';
import { expect, test } from './support/test';

const pages = [
	{
		stage: 'discover',
		op: 'getInsightsDiscover',
		fixture: discoverFixture,
		kpi: 'reported',
		value: '900'
	},
	{ stage: 'fetch', op: 'getInsightsFetch', fixture: fetchFixture, kpi: 'fetched', value: '4,321' },
	{
		stage: 'understand',
		op: 'getInsightsUnderstand',
		fixture: understandFixture,
		kpi: 'stored',
		value: '3,579'
	},
	{
		stage: 'index',
		op: 'getInsightsIndex',
		fixture: indexFixture,
		kpi: 'searchable',
		value: '1,234,567'
	},
	{ stage: 'serve', op: 'getInsightsServe', fixture: serveFixture, kpi: 'searches', value: '8,642' }
] as const;

for (const p of pages) {
	test(`${p.stage}: renders KPIs, chart, context and passes axe`, async ({ page, mock, go }) => {
		mock.on(p.op, p.fixture());
		await go(`/insights/${p.stage}?range=30d&compare=1`);
		await expect(page.getByTestId(`insights-stage-${p.kpi}-value`).first()).toHaveText(p.value);
		await expect(page.getByTestId('insights-stage-kpis')).toBeVisible();
		// Context is kept: the breadcrumb and the other stages carry range and compare.
		const back = await page.getByTestId('insights-back').getAttribute('href');
		expect(back).toContain('range=30d');
		expect(back).toContain('compare=1');
		const other = p.stage === 'fetch' ? 'serve' : 'fetch';
		expect(await page.getByTestId(`insights-stage-${other}`).getAttribute('href')).toContain(
			'range=30d'
		);
		await expect(page.getByTestId(`insights-stage-${p.stage}`)).toHaveAttribute(
			'aria-current',
			'page'
		);
		await expectNoA11yViolations(page);
		const q = new URL(mock.calls(p.op)[0]!.url()).searchParams;
		expect(q.get('range')).toBe('30d');
		expect(q.get('compare')).toBe('1');
	});
}

test('a missing series reads "Not available yet", never 0', async ({ page, mock, go }) => {
	mock.on('getInsightsFetch', fetchFixture());
	await go('/insights/fetch');
	await expect(page.getByTestId('insights-stage-utp_share-na')).toHaveText('Not available yet');
	await expect(page.getByTestId('insights-stage-utp_share-value')).toHaveText('—');
});

test('discover: unique harvested is ≈, unmeasured series are not available', async ({
	page,
	mock,
	go
}) => {
	mock.on('getInsightsDiscover', discoverFixture());
	await go('/insights/discover');
	await expect(page.getByTestId('insights-stage-harvested-value').first()).toHaveText(/^≈\s8,765$/);
	await expect(page.getByTestId('insights-stage-bep51-na')).toHaveText('Not available yet');
	await expect(page.getByTestId('insights-table-by-ip-family-na')).toBeVisible();
	await expect(page.getByTestId('insights-table-by-source')).toContainText('dht');
	await expect(page.getByRole('link', { name: 'Download CSV' }).first()).toBeVisible();
});

test('fetch: a loss links to its reason and the chip resets it', async ({ page, mock, go }) => {
	mock.on('getInsightsFetch', fetchFixture());
	await go('/insights/fetch');
	await page.getByRole('link', { name: 'No peers found' }).click();
	await expect(page).toHaveURL(/reason=no_peers/);
	await expect(page.getByTestId('insights-reason-chip')).toBeVisible();
	await expect
		.poll(() => mock.calls('getInsightsFetch').some((r) => r.url().includes('reason=no_peers')))
		.toBe(true);
	await page.getByTestId('insights-reason-chip').click();
	await expect(page).not.toHaveURL(/reason=/);
});

test('fetch: the reason survives a range or Compare change; an unknown reason is ignored', async ({
	page,
	mock,
	go
}) => {
	mock.on('getInsightsFetch', fetchFixture());
	await go('/insights/fetch?reason=no_peers');
	await expect(page.getByTestId('insights-stage-compare')).toHaveAttribute('aria-pressed', 'false');
	await page.getByTestId('insights-stage-compare').click();
	await expect(page).toHaveURL(/compare=1/);
	await expect(page).toHaveURL(/reason=no_peers/);
	await expect(page.getByTestId('insights-stage-compare')).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByRole('radio', { name: '7 d' })).toBeChecked();
	await go('/insights/fetch?reason=bogus');
	await expect(page.getByTestId('insights-reason-chip')).toHaveCount(0);
	await expect(page.getByTestId('insights-stage-kpis')).toBeVisible();
});

test('fetch: null losses read "Not available yet"', async ({ page, mock, go }) => {
	mock.on('getInsightsFetch', fetchFixture({ losses: null }));
	await go('/insights/fetch');
	await expect(page.getByTestId('insights-bars-losses-na')).toHaveText('Not available yet');
});

test('index: a stale banner shows and missing sizes are not zeros', async ({ page, mock, go }) => {
	mock.on('getInsightsIndex', indexFixture());
	await go('/insights/index');
	await expect(page.getByTestId('insights-stale')).toBeVisible();
	await expect(page.getByTestId('insights-stage-db-na')).toHaveText('Not available yet');
	await expect(page.getByTestId('insights-table-trackers-na')).toBeVisible();
	await expect(page.getByTestId('insights-stage-note-rebuild')).toHaveText('Building');
});

test('serve: top queries appear only when logging is on', async ({ page, mock, go }) => {
	mock.on('getInsightsServe', serveFixture());
	await go('/insights/serve');
	await expect(page.getByTestId('insights-table-top-queries-na')).toBeVisible();
	mock.on(
		'getInsightsServe',
		serveFixture({ top_queries: [{ count: 12, query: 'ubuntu iso', zero: 0 }] })
	);
	await page.getByRole('radio', { name: '24 h' }).click();
	await expect(page.getByTestId('insights-table-top-queries')).toContainText('ubuntu iso');
});

test('an error keeps the context usable and retry reloads', async ({ page, mock, go }) => {
	mock.error('getInsightsServe', 500, 'internal', 'boom');
	await go('/insights/serve');
	await expect(page.getByTestId('insights-retry')).toBeVisible();
	await expect(page.getByTestId('insights-back')).toBeVisible();
	mock.on('getInsightsServe', serveFixture());
	await page.getByTestId('insights-retry').click();
	await expect(page.getByTestId('insights-stage-searches-value').first()).toHaveText('8,642');
});

test('keyboard: breadcrumb back to the overview keeps the context', async ({
	page,
	mock,
	go,
	kbd
}) => {
	mock.on('getInsightsFetch', fetchFixture());
	await go('/insights/fetch?range=24h');
	await kbd.tabTo(page.getByTestId('insights-back'));
	await expect(page.getByTestId('insights-back')).toHaveAttribute('href', /range=24h/);
});

test('keyboard: focus stays on the range group and on Compare after they navigate', async ({
	page,
	mock,
	go
}) => {
	mock.on('getInsightsFetch', fetchFixture());
	await go('/insights/fetch?range=7d');
	const group = page.getByRole('radiogroup', { name: 'Time range' });
	await page.getByRole('radio', { name: '7 d' }).focus();
	await page.keyboard.press('ArrowRight');
	await page.keyboard.press('ArrowRight');
	await expect(page).toHaveURL(/range=all/);
	await expect(group.getByRole('radio', { checked: true })).toBeFocused();
	await go('/insights/fetch?range=7d');
	const compare = page.getByTestId('insights-stage-compare');
	await compare.focus();
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL(/compare=1/);
	await expect(compare).toBeFocused();
});

test.describe('under a coarse pointer', () => {
	test.use({ hasTouch: true, isMobile: true, viewport: { width: 390, height: 800 } });
	test('the stage links on the overview are 44 px targets', async ({ page, mock, go }) => {
		mock.on('getInsightsOverview', overviewFixture());
		mock.on('getCrawler', crawlerFixture());
		await go('/insights');
		for (const stage of ['discover', 'fetch', 'understand', 'index', 'serve']) {
			const box = (await page.getByTestId(`insights-detail-${stage}`).boundingBox())!;
			expect(box.height).toBeGreaterThanOrEqual(44);
		}
	});
});
