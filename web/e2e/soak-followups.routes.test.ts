import { body, expect, hits, row, server } from './search-results.fixtures';
import { test } from './support/test';
import { crawlerFixture, overviewFixture } from '../src/lib/features/insights/overview/fixtures';
import { newest, response, hit, showAdultSettings } from '../src/lib/features/discover/testing';

for (const stored of [560, 640]) {
	test(`detail fits at 1366 with saved width ${stored}`, async ({ page, go, mock }) => {
		await page.setViewportSize({ width: 1366, height: 900 });
		await page.addInitScript(
			(width) => localStorage.setItem('lodestar.drawer.width', String(width)),
			stored
		);
		server(mock, () => body(hits(3)));
		await go('/search?q=lantern');
		await row(page, 1).click();
		await expect(page.locator('.ls-detail-host')).toHaveAttribute('data-mode', 'overlay');
		await expect
			.poll(async () => {
				const box = await page.getByTestId('drawer').boundingBox();
				return box ? box.x >= 0 && box.x + box.width <= 1367 : false;
			})
			.toBe(true);
		await expect(page.getByRole('button', { name: 'Close details' })).toBeInViewport();
		await page.setViewportSize({ width: 1920, height: 900 });
		await expect(page.locator('.ls-detail-host')).toHaveAttribute('data-mode', 'inline');
		await page.getByTestId('query-editor').focus();
		await expect(page.getByTestId('query-editor')).toBeFocused();
	});
}

test('fullscreen detail covers background focus and releases it on resize and close', async ({
	page,
	go,
	mock
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	server(mock, () => body(hits(3)));
	await go('/search?q=lantern');
	await row(page, 1).click();
	const drawer = page.getByTestId('drawer');
	await expect(drawer).toBeVisible();
	await expect
		.poll(() => page.locator('#results').evaluate((el: HTMLElement) => el.inert))
		.toBe(true);
	await drawer.getByRole('heading', { level: 2 }).focus();
	await page.keyboard.press('Shift+Tab');
	expect(await page.evaluate(() => !!document.activeElement?.closest('[inert]'))).toBe(false);
	await page.getByTestId('query-editor').evaluate((el) => el.focus());
	await expect(page.getByTestId('query-editor')).not.toBeFocused();
	await page.setViewportSize({ width: 1920, height: 900 });
	await expect
		.poll(() => page.locator('#results').evaluate((el: HTMLElement) => el.inert))
		.toBe(false);
	await page.getByTestId('query-editor').focus();
	await expect(page.getByTestId('query-editor')).toBeFocused();
	await page.setViewportSize({ width: 390, height: 844 });
	await page.goBack();
	await expect(drawer).toHaveCount(0);
	await page.getByTestId('query-editor').click();
	await expect(page.getByTestId('query-editor')).toBeFocused();
});

for (const width of [320, 390, 1366]) {
	test(`Insights jump clears the wrapped context bar at ${width}`, async ({ page, go, mock }) => {
		await page.setViewportSize({ width, height: 900 });
		mock.on('getInsightsOverview', overviewFixture());
		mock.on('getCrawler', crawlerFixture());
		await go('/insights?range=custom&from=1700000000&to=1700600000');
		await page.getByTestId('insights-stage-fetch').click();
		const heading = page.locator('#insights-funnel-heading');
		await expect(heading).toBeFocused();
		await expect
			.poll(async () => {
				const h = await heading.boundingBox();
				const bar = await page.getByTestId('insights-bar').boundingBox();
				return !!h && !!bar && h.y >= bar.y + bar.height;
			})
			.toBe(true);
	});
}

test('short Discover lists end at their loaded rows', async ({ page, go, mock }) => {
	await page.setViewportSize({ width: 390, height: 900 });
	mock.on('getSettings', showAdultSettings(false));
	mock.on(
		'getDiscover',
		response({ newest: newest({ hits: [hit(1), hit(2), hit(3)], total: 3, next_cursor: null }) })
	);
	await go('/discover');
	await expect(page.locator('.dsc-list')).toBeVisible();
	await expect
		.poll(async () => (await page.locator('.dsc-list').boundingBox())?.height ?? Infinity)
		.toBeLessThanOrEqual(193);
});

test('narrow result rows retain size and swarm facts', async ({ page, go, mock }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	server(mock, () => body(hits(3)));
	await go('/search?q=lantern');
	const first = row(page, 1);
	for (const suffix of ['size', 'swarm']) {
		const fact = first.locator(`[data-testid$="-${suffix}"]`);
		await expect(fact).toBeVisible();
		const rect = await fact.boundingBox();
		const main = await first.locator('.rr-main').boundingBox();
		expect(rect!.x + rect!.width).toBeLessThanOrEqual(main!.x + main!.width + 1);
	}
});

test('unfinished settings have a clear alternative and return path', async ({ page, go, mock }) => {
	mock.on('getInsightsOverview', overviewFixture());
	mock.on('getCrawler', crawlerFixture());
	await go('/settings');
	await expect(page.getByTestId('unavailable-page')).toContainText('not available in this build');
	await page.getByRole('link', { name: 'Go to Insights' }).click();
	await expect(page.getByTestId('insights-page')).toBeVisible();
});
