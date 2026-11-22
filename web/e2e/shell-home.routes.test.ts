// X-shell-home against the OpenAPI mock: the Search home screen (what it is, what to type, recent
// searches kept in this browser) and the top bar's words, with no layout shift at 1440 and 390 px.
import { expectNoA11yViolations } from './a11y';
import { indexState } from './search-results.fixtures';
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
			await expect(page.getByTestId('results-empty-title')).toHaveText('Search 2,400,000 torrents');
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
