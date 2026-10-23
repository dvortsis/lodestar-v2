// S-discover: /discover against the OpenAPI mock. Every answer is typed from schema.d.ts.
import { expectNoA11yViolations } from './a11y';
import type { OpenApiMock } from './mock/openapi-mock';
import { expect, test } from './support/test';
import {
	discoverResponse,
	group,
	hash,
	hit,
	newest,
	response,
	showAdultSettings,
	unavailable
} from '../src/lib/features/discover/testing';

const typeOf = (request: { url(): string }) => new URL(request.url()).searchParams.get('type');
const cursorOf = (request: { url(): string }) => new URL(request.url()).searchParams.get('cursor');
const page1 = Array.from({ length: 50 }, (_, i) => hit(i + 1));

/** A mock whose Newest answers per tab; `cursor` pages return the next ten rows. */
function serve(mock: OpenApiMock, opts: { showAdult?: boolean } = {}) {
	mock.on('getSettings', showAdultSettings(opts.showAdult ?? false));
	mock.on('getDiscover', (request) => {
		const type = typeOf(request);
		if (cursorOf(request)) {
			return response({
				newest: newest({
					hits: Array.from({ length: 10 }, (_, i) => hit(51 + i)),
					total: 60,
					type,
					offset: 50
				})
			});
		}
		return response({
			newest: newest({ hits: page1, total: 60, next_cursor: 'c1', type })
		});
	});
}

test.describe('tabs and URL state', () => {
	test('All, then a tab: URL, aria-current and a request carrying the type', async ({
		page,
		go,
		mock
	}) => {
		serve(mock);
		await go('/discover');
		await expect(page.getByRole('heading', { level: 1, name: 'Discover' })).toBeVisible();
		await expect(page.getByTestId('discover-tab-all')).toHaveAttribute('aria-current', 'page');
		await expect(page.getByTestId('discover-newest-total')).toHaveText('60');
		expect(mock.calls('getDiscover').map(typeOf)).toEqual([null]);

		await page.getByTestId('discover-tab-movie').click();
		await expect(page).toHaveURL(/[?&]type=movie/);
		await expect(page.getByTestId('discover-tab-movie')).toHaveAttribute('aria-current', 'page');
		await expect(page.getByTestId('discover-tab-all')).not.toHaveAttribute('aria-current', 'page');
		await expect.poll(() => mock.calls('getDiscover').map(typeOf)).toEqual([null, 'movie']);
		await expect(page.getByTestId('discover-see-all')).toHaveAttribute(
			'href',
			'/search?q=type%3Amovie&sort=-discovered&density=comfortable'
		);
		// Back restores the previous tab: a tab change is a history entry.
		await page.goBack();
		await expect(page.getByTestId('discover-tab-all')).toHaveAttribute('aria-current', 'page');
	});

	test('density stays in the URL when the tab changes', async ({ page, mock }) => {
		serve(mock);
		await page.goto('/discover?density=compact');
		await page.getByTestId('discover-tab-tv').click();
		await expect(page).toHaveURL(/density=compact/);
		await expect(page).toHaveURL(/type=tv/);
	});

	test('an unknown type says so and links to All', async ({ page, go, mock }) => {
		mock.on('getSettings', showAdultSettings(false));
		mock.error('getDiscover', 400, 'unknown_type', 'Unknown type', { field: 'type' });
		await go('/discover?type=bogus');
		await expect(page.getByRole('heading', { name: "That type doesn't exist" })).toBeVisible();
		await page.getByRole('link', { name: 'Show all types' }).click();
		await expect(page).toHaveURL(/\/discover(\?density=comfortable)?$/);
	});
});

test.describe('paging', () => {
	test('scrolling to the end asks for the next page with the same type, and appends', async ({
		page,
		go,
		mock
	}) => {
		serve(mock);
		await go('/discover?type=movie');
		const list = page.getByRole('listbox', { name: 'Newest torrents' });
		await expect(list.getByRole('option').first()).toBeVisible();
		await list.evaluate((el) => (el.scrollTop = el.scrollHeight));
		await expect.poll(() => mock.calls('getDiscover').length).toBe(2);
		const [, second] = mock.calls('getDiscover');
		expect(cursorOf(second!)).toBe('c1');
		expect(typeOf(second!)).toBe('movie');
		await list.evaluate((el) => (el.scrollTop = el.scrollHeight));
		await expect(page.getByTestId(`discover-row-${hash(60)}`)).toBeVisible();
		// The last page had no cursor: no third request.
		// ...and the load-more sentinel is gone, so nothing will ask for one.
		await expect(list.locator('[role="presentation"][aria-hidden="true"]')).toHaveCount(0);
		expect(mock.calls('getDiscover')).toHaveLength(2);
	});

	test('a failed page keeps the rows, offers Retry and does not loop', async ({
		page,
		go,
		mock
	}) => {
		mock.on('getSettings', showAdultSettings(false));
		let failing = true;
		mock.on('getDiscover', (request) => {
			if (cursorOf(request)) {
				return failing
					? { status: 502, body: { error: { code: 'bad_gateway', message: 'bad gateway' } } }
					: response({ newest: newest({ hits: [hit(51)], total: 51 }) });
			}
			return response({ newest: newest({ hits: page1, total: 51, next_cursor: 'c1' }) });
		});
		await go('/discover');
		const list = page.getByRole('listbox', { name: 'Newest torrents' });
		await expect(list.getByRole('option').first()).toBeVisible();
		await list.evaluate((el) => (el.scrollTop = el.scrollHeight));
		await expect(page.getByText("Couldn't load more torrents")).toBeVisible();
		await expect(list.getByRole('option').first()).toBeVisible(); // stale rows stay
		// hasMore is cleared: the sentinel that would re-ask is gone, so there is no retry loop.
		await expect(list.locator('[role="presentation"][aria-hidden="true"]')).toHaveCount(0);
		expect(mock.calls('getDiscover')).toHaveLength(2);
		failing = false;
		await page.getByRole('button', { name: 'Try again' }).click();
		await expect(page.getByText("Couldn't load more torrents")).toBeHidden();
	});
});

test.describe('states', () => {
	test('nothing discovered yet (All) and no <type> yet (a tab)', async ({ page, go, mock }) => {
		mock.on('getSettings', showAdultSettings(false));
		mock.on('getDiscover', response({ newest: newest({ hits: [], total: 0 }) }));
		await go('/discover');
		await expect(page.getByRole('heading', { name: 'Nothing discovered yet' })).toBeVisible();
		await expect(
			page.getByRole('main').getByRole('link', { name: 'Open Insights' })
		).toHaveAttribute('href', '/insights');
		await go('/discover?type=comic');
		await expect(page.getByRole('heading', { name: 'No comic torrents yet' })).toBeVisible();
		await expect(page.getByTestId('discover-newest-total')).toHaveText('0');
	});

	test('search turned off', async ({ page, go, mock }) => {
		mock.on('getSettings', showAdultSettings(false));
		mock.error('getDiscover', 503, 'search_disabled', 'Search is disabled');
		await go('/discover');
		await expect(page.getByRole('heading', { name: 'Search is turned off' })).toBeVisible();
		await expect(page.getByRole('link', { name: 'Open search settings' })).toHaveAttribute(
			'href',
			'/settings/search'
		);
	});

	test('a network error says what happened and Retry loads it', async ({ page, go, mock }) => {
		mock.on('getSettings', showAdultSettings(false));
		mock.error('getDiscover', 500, 'internal', 'boom');
		await go('/discover');
		await expect(page.getByText(/Couldn't load Discover/)).toBeVisible();
		mock.on('getDiscover', discoverResponse());
		await page.getByRole('button', { name: 'Try again' }).click();
		await expect(page.getByTestId('discover-newest-total')).toHaveText('3');
		await expect(page.getByText(/Couldn't load Discover/)).toBeHidden();
	});

	test('Rising and Recently resolved: unavailable as notices, available as rows', async ({
		page,
		go,
		mock
	}) => {
		mock.on('getSettings', showAdultSettings(false));
		mock.on('getDiscover', discoverResponse());
		await go('/discover');
		await expect(page.getByTestId('discover-rising-unavailable')).toContainText(
			'enough seeder counts'
		);
		await expect(page.getByTestId('discover-resolved-unavailable')).toContainText(
			"doesn't record when a torrent gets identified"
		);
		mock.on(
			'getDiscover',
			discoverResponse({
				rising: group({ hits: [hit(21)] }),
				recently_resolved: unavailable('search_disabled')
			})
		);
		await go('/discover');
		await expect(page.getByTestId(`discover-rising-${hash(21)}`)).toHaveAttribute(
			'href',
			`/t/${hash(21)}?density=comfortable`
		);
		await expect(page.getByTestId('discover-resolved')).toContainText('Turn on search');
	});

	test('partial: ≈ total, never-scraped dash, empty composition', async ({ page, go, mock }) => {
		mock.on('getSettings', showAdultSettings(false));
		mock.on(
			'getDiscover',
			response({
				newest: newest({
					hits: [
						hit(1, {
							seeders: null,
							leechers: null,
							trackers_reporting: null,
							trackers_checked: null
						}),
						hit(2, { composition: [], title: null })
					],
					total: 2_400_000,
					total_approx: true
				})
			})
		);
		await go('/discover');
		await expect(page.getByTestId('discover-newest-total')).toHaveText('≈ 2.4M');
		await expect(page.getByTestId(`discover-row-${hash(1)}-never-scraped`)).toHaveAccessibleName(
			'Never scraped'
		);
		await expect(page.getByTestId(`discover-row-${hash(2)}`)).toContainText('Not classified yet');
		await expect(page.getByTestId(`discover-row-${hash(2)}-title`)).toHaveText('Raw.Name.2.2160p');
	});
});

test.describe('Adult', () => {
	test('the tab is absent while Show adult content is off', async ({ page, go, mock }) => {
		serve(mock, { showAdult: false });
		await go('/discover');
		await expect(page.getByTestId('discover-newest-total')).toBeVisible();
		await expect(page.getByTestId('discover-tab-adult')).toHaveCount(0);
	});

	test('the tab shows when it is on', async ({ page, go, mock }) => {
		serve(mock, { showAdult: true });
		await go('/discover');
		await expect(page.getByTestId('discover-tab-adult')).toBeVisible();
	});

	test('type=adult while hidden shows the server notice, never an empty zero', async ({
		page,
		go,
		mock
	}) => {
		mock.on('getSettings', showAdultSettings(false));
		mock.on(
			'getDiscover',
			response({
				newest: newest({ hits: [], total: 0, notices: ['adult_hidden'], type: 'adult' })
			})
		);
		await go('/discover?type=adult');
		await expect(page.getByText(/Adult content is hidden/)).toBeVisible();
		await expect(page.getByRole('link', { name: 'Open settings' })).toHaveAttribute(
			'href',
			'/settings/content'
		);
		await expect(page.getByTestId('discover-newest-total')).toHaveCount(0);
		await expect(page.getByRole('heading', { name: /No adult torrents/ })).toHaveCount(0);
	});
});

test.describe('widths', () => {
	for (const [width, height] of [
		[390, 800],
		[768, 900],
		[1366, 800]
	] as const) {
		test(`${width} px: no horizontal page scroll, rows at their fixed height, axe clean`, async ({
			page,
			go,
			mock
		}) => {
			await page.setViewportSize({ width, height });
			serve(mock);
			await go('/discover');
			const first = page.getByTestId(`discover-row-${hash(1)}`);
			await expect(first).toBeVisible();
			expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(
				width
			);
			// Comfortable rows are 64 px (--row-height-comfortable), Compact 36 px, both fixed.
			const box = await first.boundingBox();
			expect(box!.height).toBeCloseTo(64, 0);
			// Nothing inside the fixed-height row is clipped (title, raw name and facts all fit).
			expect(
				await first.locator('.dsc-row').evaluate((el) => el.scrollHeight - el.clientHeight)
			).toBeLessThanOrEqual(0);
			// Tabs scroll inside their own box, not the page.
			await expect(page.getByRole('navigation', { name: 'Type' })).toBeVisible();
			await expectNoA11yViolations(page);
		});
	}

	test('Compact rows are one 36 px line', async ({ page, mock }) => {
		serve(mock);
		await page.goto('/discover?density=compact');
		const box = await page.getByTestId(`discover-row-${hash(1)}`).boundingBox();
		expect(box!.height).toBeCloseTo(36, 0);
	});
});

for (const [themeId, colorMode] of [
	['riverstone-light', 'light'],
	['riverstone-dark', 'dark']
] as const) {
	test.describe(`axe, ${themeId}`, () => {
		test.use({ themeId, colorMode });

		test('populated', async ({ page, go, mock }) => {
			mock.on('getSettings', showAdultSettings(true));
			mock.on(
				'getDiscover',
				discoverResponse({
					rising: group({ hits: [hit(21)] }),
					recently_resolved: unavailable('no_title')
				})
			);
			await go('/discover');
			await expect(page.getByTestId(`discover-row-${hash(1)}`)).toBeVisible();
			await expectNoA11yViolations(page);
		});

		test('empty', async ({ page, go, mock }) => {
			mock.on('getSettings', showAdultSettings(false));
			mock.on('getDiscover', response({ newest: newest({ hits: [], total: 0 }) }));
			await go('/discover');
			await expect(page.getByRole('heading', { name: 'Nothing discovered yet' })).toBeVisible();
			await expectNoA11yViolations(page);
		});

		test('error', async ({ page, go, mock }) => {
			mock.on('getSettings', showAdultSettings(false));
			mock.error('getDiscover', 500, 'internal', 'boom');
			await go('/discover');
			await expect(page.getByText(/Couldn't load Discover/)).toBeVisible();
			await expectNoA11yViolations(page);
		});
	});
}

test.describe('the first-load reveal', () => {
	test('the first rows animate, 80 ms apart, once', async ({ page, go, mock }) => {
		serve(mock);
		await go('/discover');
		const animated = page.locator('.dsc-reveal');
		await expect(animated.first()).toBeVisible();
		expect(await animated.count()).toBeLessThanOrEqual(8);
		const delays = await animated.evaluateAll((els) =>
			els.map((el) => getComputedStyle(el, '::after').animationDelay)
		);
		expect(delays[0]).toBe('0s');
		expect(delays[1]).toBe('0.08s');
		// Nothing is withheld: the rows are focusable from the start.
		await expect(page.getByTestId(`discover-row-${hash(1)}`)).toBeVisible();
		await page.getByTestId('discover-tab-movie').click();
		await expect(page.locator('.dsc-reveal')).toHaveCount(0);
	});

	test.describe('reduced motion', () => {
		test.use({ contextOptions: { reducedMotion: 'reduce' }, motion: 'reduce' });
		test('nothing animates', async ({ page, go, mock }) => {
			serve(mock);
			await go('/discover');
			await expect(page.getByTestId(`discover-row-${hash(1)}`)).toBeVisible();
			await expect(page.locator('.dsc-reveal')).toHaveCount(0);
			const names = await page
				.getByTestId(`discover-row-${hash(1)}`)
				.evaluate((el) => getComputedStyle(el).animationName);
			expect(names).toBe('none');
		});
	});
});
