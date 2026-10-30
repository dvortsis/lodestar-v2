// The stalled-pipeline warning (build-plan F-status): the pill and the Insights overview say so
// when the server reports a stall, at a phone and a desktop width. Name ends in `routes.test.ts`
// so the `core` project runs it.
import { liveEvents } from './support/events-server';
import { expect, test } from './support/test';
import { overviewFixture } from '../src/lib/features/insights/overview/fixtures';

const stats = {
	data_bytes: 1,
	disk_free_bytes: null,
	fetched_per_hour: 0,
	harvested_per_hour: 0,
	index_lag_s: null,
	paused: false,
	queue_depth: 10,
	sampling_blocked: null,
	searchable_torrents: 5,
	stored_torrents: 5,
	updated_at: 1792681200,
	uptime_s: 86_400
};

for (const width of [390, 1366]) {
	test.describe(`stalled pipeline at ${width} px`, () => {
		test.beforeEach(async ({ page, mock }) => {
			await page.setViewportSize({ width, height: 800 });
			mock.on('getStats', stats);
			mock.on(
				'getInsightsOverview',
				overviewFixture({
					ingest_per_hour: 0,
					exceptions: [
						{
							fix: { kind: 'stage', target: 'fetch' },
							kind: 'cohort_stalled',
							severity: 'warning',
							since: 1792670000,
							unit: 'torrents',
							value: 1200
						}
					]
				})
			);
		});

		test('the pill warns, with words, and links to Insights', async ({ page, go }) => {
			await liveEvents(page);
			await go('/search');
			const pill = page.getByTestId('status-pill');
			await expect.poll(() => pill.getAttribute('data-state'), { timeout: 15_000 }).toBe('stalled');
			await expect(pill).toHaveAttribute('href', '/insights');
			await expect(pill).toContainText('Stalled'); // in the accessible name at every width
			await expect(page.getByTestId('status-pill-announce')).toContainText('Open Insights', {
				timeout: 15_000
			});
			if (width >= 1024) await expect(page.getByTestId('status-pill-text')).toBeVisible();
		});

		test('Insights shows the same warning at the top', async ({ page, go }) => {
			await liveEvents(page);
			await go('/insights');
			await expect(page.getByTestId('insights-stalled')).toBeVisible({ timeout: 15_000 });
			await expect(page.getByText('The crawler looks stalled')).toBeVisible();
		});
	});
}

test('a healthy overview shows no stalled warning on Insights', async ({ page, go, mock }) => {
	await liveEvents(page);
	mock.on('getStats', { ...stats, fetched_per_hour: 1200 });
	mock.on('getInsightsOverview', overviewFixture({ exceptions: [] }));
	await go('/insights');
	// The overview has rendered (the skeleton has no status section) before "none" means anything.
	await expect(page.locator('#insights-status')).toBeVisible({ timeout: 15_000 });
	await expect(page.getByTestId('insights-stalled')).toHaveCount(0);
});
