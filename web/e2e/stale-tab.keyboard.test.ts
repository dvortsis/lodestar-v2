// "A newer Lodestar is running" (docs/decisions/2026-11-01-X-stale-tab.md): the shell compares this
// tab's UI build with the server's `_app/version.json` when the tab is shown again, at most every
// five minutes, and offers a reload. Condition waits only.
import { liveEvents } from './support/events-server';
import { expectNoA11yViolations } from './a11y';
import { expect, test } from './support/test';
import { crawlerFixture, overviewFixture } from '../src/lib/features/insights/overview/fixtures';

test.beforeEach(async ({ page, mock }) => {
	await liveEvents(page);
	mock.on('getInsightsOverview', overviewFixture());
	mock.on('getCrawler', crawlerFixture());
	mock.on('listSavedSearches', []);
	// The five-minute throttle is read from Date.now: the page can be moved past it.
	await page.addInitScript(() => {
		const real = Date.now.bind(Date);
		let skew = 0;
		Date.now = () => real() + skew;
		(window as unknown as { skewMinutes: (n: number) => void }).skewMinutes = (n) =>
			(skew += n * 60_000);
	});
});

const shown = (page: import('@playwright/test').Page) =>
	page.evaluate(() => {
		(window as unknown as { skewMinutes: (n: number) => void }).skewMinutes(6);
		document.dispatchEvent(new Event('visibilitychange'));
	});

test('a different server build shows the banner, axe clean, Reload reachable by keyboard', async ({
	page,
	go,
	kbd
}) => {
	let asked = 0;
	await page.route('**/_app/version.json', (route) => {
		asked++;
		return route.fulfill({ json: { version: 'another-build' } });
	});
	await go('/search');
	// The check starts with the chrome, which loads on the first interaction.
	await expect(page.getByTestId('status-pill')).toBeVisible();
	await expect.poll(() => shown(page).then(() => asked), { timeout: 15_000 }).toBeGreaterThan(0);
	const banner = page.getByTestId('stale-banner');
	await expect(banner).toContainText('A newer version of Lodestar is running. Reload to update.');
	await expect(banner).not.toContainText('Save or discard');
	await expectNoA11yViolations(page);
	await kbd.tabTo(page.getByRole('button', { name: 'Reload' }), 80);
});

test("the banner shows although the old build's chunks are gone from the new server", async ({
	page,
	go
}) => {
	let asked = 0;
	await page.route('**/_app/version.json', (route) => {
		asked++;
		return route.fulfill({ json: { version: 'another-build' } });
	});
	await go('/search');
	await expect(page.getByTestId('status-pill')).toBeVisible();
	// The new server answers any `_app/` file of the old build with a 404.
	await page.route('**/_app/immutable/**', (route) => route.fulfill({ status: 404, body: '' }));
	await expect.poll(() => shown(page).then(() => asked), { timeout: 15_000 }).toBeGreaterThan(0);
	await expect(page.getByTestId('stale-banner')).toBeVisible();
});

test('the same build shows nothing', async ({ page, go }) => {
	let asked = 0;
	await page.route('**/_app/version.json', (route) => {
		asked++;
		return route.continue();
	});
	await go('/search');
	await expect(page.getByTestId('status-pill')).toBeVisible();
	await expect.poll(() => shown(page).then(() => asked), { timeout: 15_000 }).toBeGreaterThan(0);
	await expect(page.getByTestId('stale-banner')).toHaveCount(0);
});
