// IMP-5 on the screens: the crawled/imported split, "+ N imported", and the import markers on the
// overview and the Index page, against the OpenAPI mock.
import { expectNoA11yViolations } from './a11y';
import {
	crawlerFixture,
	IMPORT_ANNOTATIONS,
	overviewFixture
} from '../src/lib/features/insights/overview/fixtures';
import { indexFixture } from '../src/lib/features/insights/stages/fixtures';
import { expect, test } from './support/test';

test('overview: split, "+ N imported", delta note, markers by keyboard, axe', async ({
	page,
	mock,
	go,
	kbd
}) => {
	const base = overviewFixture();
	mock.on(
		'getInsightsOverview',
		overviewFixture({
			annotations: IMPORT_ANNOTATIONS,
			kpi: { ...base.kpi, new_searchable_imported: 250_000, searchable_delta: 250_000 }
		})
	);
	mock.on('getCrawler', crawlerFixture());
	await go('/insights');
	await expect(page.getByTestId('insights-kpi-crawled')).toHaveText('1,230,246');
	await expect(page.getByTestId('insights-kpi-imported')).toHaveText('4,321');
	await expect(page.getByTestId('insights-kpi-new-imported')).toHaveText('+ 250,000 imported');
	await expect(page.getByTestId('insights-kpi-delta-import')).toHaveText('Includes an import');
	const marker = page.getByTestId('insights-annotation-import_finished');
	await expect(marker).toContainText('Import finished: 250,000 new, 1,234 already known');
	await kbd.tabTo(marker);
	await expect(marker).toBeFocused();
	await expectNoA11yViolations(page);
});

test('overview: without an import there is no imported clutter', async ({ page, mock, go }) => {
	mock.on('getInsightsOverview', overviewFixture());
	mock.on('getCrawler', crawlerFixture());
	await go('/insights');
	await expect(page.getByTestId('insights-kpis')).toBeVisible();
	await expect(page.getByTestId('insights-kpi-new-imported')).toHaveCount(0);
	await expect(page.getByTestId('insights-kpi-delta-import')).toHaveCount(0);
});

test('index: Imported beside New searchable, and the removal marker', async ({
	page,
	mock,
	go,
	kbd
}) => {
	const base = indexFixture();
	mock.on('getInsightsIndex', indexFixture({ kpi: { ...base.kpi, new_searchable_imported: 900 } }));
	mock.on('getInsightsEvents', { annotations: IMPORT_ANNOTATIONS, paused: [], truncated: false });
	await go('/insights/index');
	await expect(page.getByTestId('insights-stage-new-value').first()).toHaveText('1,357');
	await expect(page.getByTestId('insights-stage-new_imported-value').first()).toHaveText('900');
	const marker = page.getByTestId('insights-import-marker-import_removed');
	await expect(marker).toContainText('Import removed: 4,321 deleted');
	await kbd.tabTo(marker);
	await expect(marker).toBeFocused();
	await expectNoA11yViolations(page);
});
