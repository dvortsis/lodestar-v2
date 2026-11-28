// X-shell-home against the OpenAPI mock: the Search home screen (what it is, what to type, recent
// searches kept in this browser) and the top bar's words, with no layout shift at 1440 and 390 px.
import { expectNoA11yViolations } from './a11y';
import { indexState } from './search-results.fixtures';
import { crawlerEvent, liveEvents } from './support/events-server';
import { interact } from './support/interact';
import { expect, test } from './support/test';
import type { OpenApiMock } from './mock/openapi-mock';

const RECENT_KEY = 'lodestar.search.recent';

function bare(mock: OpenApiMock) {
	mock.on('parseQuery', { ast: { op: 'all' }, canonical: '' });
	mock.on('searchIndexState', indexState(2_400_000));
	mock.on('listSavedSearches', []);
}

for (const width of [1440, 390]) {
	test.describe(`at ${width} px`, () => {
		test.use({ viewport: { width, height: 900 } });

		test('the home screen and the top bar render without a layout shift', async ({
			page,
			go,
			mock
		}) => {
			bare(mock);
			await page.addInitScript((key) => {
				localStorage.setItem(key, JSON.stringify(['big buck bunny', 'night sky']));
				const w = window as unknown as { __cls: number };
				w.__cls = 0;
				new PerformanceObserver((list) => {
					for (const e of list.getEntries() as unknown as {
						value: number;
						hadRecentInput: boolean;
					}[]) {
						if (!e.hadRecentInput) w.__cls += e.value;
					}
				}).observe({ type: 'layout-shift', buffered: true });
			}, RECENT_KEY);
			await go('/search');
			await interact(page);
			await expect(page.getByTestId('results-empty-title')).toHaveText(
				'Search 2,400,000 searchable torrents'
			);
			await expect(page.getByTestId('home-recent').getByRole('link')).toHaveCount(2);
			await expect(page.getByTestId('status-pill')).toBeVisible();
			const cls = await page.evaluate(() => (window as unknown as { __cls: number }).__cls);
			expect(cls).toBeLessThan(0.001);
			await expectNoA11yViolations(page);
		});
	});
}

test('recent searches can be cleared, and stay cleared', async ({ page, go, mock }) => {
	bare(mock);
	await page.addInitScript((key) => {
		if (!sessionStorage.getItem('seeded')) {
			sessionStorage.setItem('seeded', '1');
			localStorage.setItem(key, JSON.stringify(['big buck bunny']));
		}
	}, RECENT_KEY);
	await go('/search');
	await interact(page);
	await expect(page.getByTestId('home-recent').getByRole('link')).toHaveCount(1);
	await page.getByTestId('home-recent-clear').click();
	await expect(page.getByTestId('home-recent')).toHaveCount(0);
	expect(await page.evaluate((k) => localStorage.getItem(k), RECENT_KEY)).toBeNull();
});

// A dropdown menu is not modal: it must not lock the page's scroll, or a classic scrollbar vanishes
// and the page moves by its width (UI review P5). Esc and an outside click still close it.
test('the overflow menu leaves the page scrollable, and closes with Esc or a click outside', async ({
	page,
	go,
	mock
}) => {
	bare(mock);
	await go('/search');
	await interact(page);
	const more = page.getByTestId('more-button');
	const overflow = () => page.evaluate(() => getComputedStyle(document.body).overflow);
	const brandX = () => page.getByTestId('brand').evaluate((el) => el.getBoundingClientRect().x);
	const before = await brandX();
	await more.click();
	await expect(page.getByRole('menu')).toBeVisible();
	expect(await overflow()).not.toBe('hidden');
	expect(await brandX()).toBe(before);
	await page.keyboard.press('Escape');
	await expect(page.getByRole('menu')).toHaveCount(0);
	await expect(more).toBeFocused();
	await more.click();
	await expect(page.getByRole('menu')).toBeVisible();
	await page.mouse.click(700, 600);
	await expect(page.getByRole('menu')).toHaveCount(0);
});

// The health line arrives with the stream and changes with the crawler. The stream is silent at
// first (the line is empty), then says "working", then "paused" (which wraps on a phone): none of it
// may move anything below the line, however late it arrives.
for (const width of [1440, 390]) {
	test(`the health line arriving and changing does not shift the page at ${width} px`, async ({
		page,
		go,
		mock
	}) => {
		await page.setViewportSize({ width, height: 900 });
		bare(mock);
		await page.addInitScript(() => {
			const w = window as unknown as { __cls: number };
			w.__cls = 0;
			new PerformanceObserver((list) => {
				for (const e of list.getEntries() as unknown as {
					value: number;
					hadRecentInput: boolean;
				}[]) {
					if (!e.hadRecentInput) w.__cls += e.value;
				}
			}).observe({ type: 'layout-shift', buffered: true });
		});
		const events = await liveEvents(page, null);
		await go('/search');
		await interact(page);
		await expect(page.getByTestId('results-empty-title')).toBeVisible();
		const health = page.getByTestId('home-health');
		await expect(health).toHaveText('');
		const examples = page.locator('.rp-examples').first();
		const y = async () => (await examples.boundingBox())!.y;
		const before = await y();
		await events.send('crawler', crawlerEvent());
		await expect(health).toContainText('Lodestar is working');
		expect(await y()).toBe(before);
		await events.send('crawler', crawlerEvent({ paused: true }));
		await expect(health).toContainText('paused');
		await expect(health.getByRole('link')).toHaveAttribute('href', '/insights');
		expect(await y()).toBe(before);
		const cls = await page.evaluate(() => (window as unknown as { __cls: number }).__cls);
		expect(cls).toBeLessThan(0.001);
	});
}
