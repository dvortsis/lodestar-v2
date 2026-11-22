// S-search-3 against the OpenAPI mock: the results toolbar (count, sort, density, Save search),
// the "Load more" fallback and end line under the list, no layout shift on a pasted link, and axe.
import type { Page } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import {
	body,
	expect,
	hits,
	indexState,
	resultsCalls,
	row,
	rowOf,
	server
} from './search-results.fixtures';
import { test } from './support/test';

const count = (page: Page) => page.getByTestId('results-count');
const sortOf = (page: Page) => new URL(page.url()).searchParams.get('sort');
const sentSorts = (mock: Parameters<typeof resultsCalls>[0]) =>
	mock
		.calls('searchPost')
		.map((r) => r.postDataJSON() as { limit: number; sort: string | null })
		.filter((b) => b.limit !== 1)
		.map((b) => b.sort);

test('the count is the server’s, ≈ and compact only when it says so', async ({
	page,
	go,
	mock
}) => {
	server(mock, (sent) =>
		JSON.stringify(sent.ast).includes('many')
			? body(hits(3), { total: 2_400_000, total_approx: true })
			: body(hits(3), { total: 1284 })
	);
	await go('/search?q=lantern');
	await expect(count(page)).toHaveText('1,284 results');
	// Visual only: the panel's polite region announces it, once.
	await expect(count(page)).not.toHaveAttribute('aria-live');
	await go('/search?q=many');
	await expect(count(page)).toHaveText('≈ 2.4M results');
});

test('sort goes to the URL and the request; Back walks it', async ({ page, go, mock }) => {
	server(mock, () => body(hits(3)));
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	const sort = page.getByLabel('Sort');
	await expect(sort).toHaveValue('relevance');
	await sort.selectOption({ label: 'Largest' });
	await expect.poll(() => sortOf(page)).toBe('-size');
	await expect.poll(() => sentSorts(mock).at(-1)).toBe('-size');
	await page.goBack();
	await expect.poll(() => sortOf(page)).toBeNull();
	await expect(sort).toHaveValue('relevance');
	// A pasted link's bare field is the descending option.
	await go('/search?q=lantern&sort=discovered');
	await expect(page.getByLabel('Sort')).toHaveValue('-discovered');
});

test('the view switches the rows and stays in the URL', async ({ page, go, mock }) => {
	server(mock, () => body(hits(3)));
	await go('/search?q=lantern');
	await expect(row(page, 1)).toHaveAttribute('data-density', 'comfortable');
	await page.getByTestId('view-select').selectOption('compact');
	await expect(page).toHaveURL(/density=compact/);
	await expect(row(page, 1)).toHaveAttribute('data-density', 'compact');
	await expect(page.getByTestId('view-select')).toHaveValue('compact');
	await page.getByTestId('view-select').selectOption('list');
	await expect(page).not.toHaveURL(/density=/);
	await expect(row(page, 1)).toHaveAttribute('data-density', 'comfortable');
});

test('Save search saves the shown query and sort, and stays on the page', async ({
	page,
	go,
	mock
}) => {
	server(mock, () => body(hits(3)));
	mock.on('createSavedSearch', (request) => ({
		status: 201,
		body: {
			alert: true,
			alerts: 0,
			ast: { op: 'all' },
			created_at: 0,
			last_alert_at: null,
			paused: false,
			q: 'lantern',
			rss: false,
			sort: '-size',
			unread: 0,
			updated_at: 0,
			id: 9,
			name: (request.postDataJSON() as { name: string }).name
		}
	}));
	await go('/search?q=lantern&sort=-size');
	await page.getByRole('button', { name: 'Save search' }).click();
	const dialog = page.getByRole('dialog', { name: 'Save search' });
	await expect(dialog.getByLabel('Query')).toHaveValue('lantern');
	await dialog.getByLabel('Name').fill('Lanterns');
	await dialog.getByRole('button', { name: 'Save search' }).click();
	await expect(page.getByText('Saved “Lanterns”')).toBeVisible();
	await expect(dialog).toHaveCount(0);
	expect(mock.calls('createSavedSearch')[0]!.postDataJSON()).toMatchObject({
		name: 'Lanterns',
		q: 'lantern',
		sort: '-size'
	});
	await expect(page).toHaveURL(/\/search\?q=lantern&sort=-size/);
});

test('Load more asks for the next page once, then the end line takes focus', async ({
	page,
	go,
	mock,
	kbd
}) => {
	await page.setViewportSize({ width: 1366, height: 900 });
	server(mock, (sent) =>
		(sent as { cursor?: string | null }).cursor === 'p2'
			? body(hits(10, 50))
			: body(hits(50), { next_cursor: 'p2', total: 60 })
	);
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	const more = page.getByRole('button', { name: 'Load more torrents' });
	await kbd.tabTo(more);
	await kbd.press('Enter');
	const end = page.getByTestId('results-end');
	await expect(end).toBeFocused();
	await expect(more).toHaveCount(0);
	expect(resultsCalls(mock).filter((c) => c.cursor === 'p2')).toHaveLength(1);
});

for (const width of [1366, 768, 390, 360, 320]) {
	test(`a pasted link does not shift as the toolbar and its count arrive at ${width} px`, async ({
		page,
		go,
		mock
	}) => {
		await page.setViewportSize({ width, height: 900 });
		server(mock, () => body(hits(3), { total: 3 }));
		// A grown index: no "still growing" banner, which a slow state read can add above the rows
		// (an existing shift that has nothing to do with the toolbar).
		mock.on('searchIndexState', indexState(5_000_000));
		await page.addInitScript(() => {
			const w = window as unknown as { __cls: number };
			w.__cls = 0;
			new PerformanceObserver((list) => {
				for (const e of list.getEntries() as unknown as {
					value: number;
					hadRecentInput: boolean;
				}[])
					if (!e.hadRecentInput) w.__cls += e.value;
			}).observe({ type: 'layout-shift', buffered: true });
		});
		await go('/search?q=lantern');
		await expect(count(page)).toHaveText('3 results');
		await expect(row(page, 1)).toBeVisible();
		const cls = await page.evaluate(() => (window as unknown as { __cls: number }).__cls);
		// Below x-large the Filters host is out of flow in every state: no shift at all. A desktop page
		// may carry a sub-pixel one (about 1e-4) that has nothing to do with it.
		expect(cls).toBeLessThan(width < 1024 ? 1e-9 : 1e-3);
	});
}

// CI answers the toolbar's chunk later than the results panel's: the Filters host must not move
// when it arrives (a slow chunk, which a fast local run hides).
for (const width of [768, 390, 320]) {
	test(`a pasted link does not shift when a slow toolbar chunk arrives at ${width} px`, async ({
		page,
		go,
		mock
	}) => {
		await page.setViewportSize({ width, height: 900 });
		server(mock, () => body(hits(3), { total: 3 }));
		// A grown index: no "still growing" banner, which a slow state read can add above the rows
		// (an existing shift that has nothing to do with the toolbar).
		mock.on('searchIndexState', indexState(5_000_000));
		await page.addInitScript(() => {
			const w = window as unknown as { __cls: number };
			w.__cls = 0;
			new PerformanceObserver((list) => {
				for (const e of list.getEntries() as unknown as {
					value: number;
					hadRecentInput: boolean;
				}[])
					if (!e.hadRecentInput) w.__cls += e.value;
			}).observe({ type: 'layout-shift', buffered: true });
		});
		// A slow CPU, as on CI: the first frames come apart.
		const cdp = await page.context().newCDPSession(page);
		await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 });
		await page.route('**/_app/immutable/**/*.js', async (route) => {
			try {
				const res = await route.fetch();
				const body = await res.body();
				if (body.includes('results-toolbar')) await new Promise((done) => setTimeout(done, 600));
				await route.fulfill({ response: res, body });
			} catch {
				// A fetch that failed under load goes to the network as it is; a closed page ignores this.
				await route.continue().catch(() => {});
			}
		});
		await go('/search?q=lantern');
		await expect(count(page)).toHaveText('3 results', { timeout: 30_000 });
		await expect(row(page, 1)).toBeVisible({ timeout: 30_000 });
		const cls = await page.evaluate(() => (window as unknown as { __cls: number }).__cls);
		// Below x-large the Filters host is out of flow in every state: no shift at all. A desktop page
		// may carry a sub-pixel one (about 1e-4) that has nothing to do with it.
		expect(cls).toBeLessThan(width < 1024 ? 1e-9 : 1e-3);
	});
}

for (const mode of ['light', 'dark'] as const) {
	test.describe(`axe, Riverstone ${mode}`, () => {
		test.use({ themeId: `riverstone-${mode}`, colorMode: mode });
		for (const width of [1366, 390]) {
			test(`the toolbar, Load more and the end line at ${width} px`, async ({ page, go, mock }) => {
				await page.setViewportSize({ width, height: 900 });
				server(mock, (sent) =>
					JSON.stringify(sent.ast).includes('more')
						? body(hits(50), { next_cursor: 'p2', total: 60 })
						: body(hits(3))
				);
				await go('/search?q=lantern');
				await expect(page.getByTestId('results-end')).toBeVisible();
				await expectNoA11yViolations(page);
				await go('/search?q=more&density=compact');
				await expect(page.getByRole('button', { name: 'Load more torrents' })).toBeVisible();
				await expectNoA11yViolations(page);
			});
		}
	});
}

test('on a phone the controls fit one row above the first result, and More is a disclosure', async ({
	page,
	go,
	mock
}) => {
	server(mock, () => body(hits(3), { total: 1284 }));
	mock.on('searchIndexState', indexState(84_210, { growing: true, growth_per_hour: 2104.5 }));
	await page.setViewportSize({ width: 390, height: 800 });
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	const filters = await page.getByTestId('facet-open').boundingBox();
	const sort = await page.getByLabel('Sort').boundingBox();
	const first = await row(page, 1).boundingBox();
	// Filters and Sort share a line, and the first row starts well above the old 515 px.
	expect(Math.abs(filters!.y - sort!.y)).toBeLessThan(16);
	expect(first!.y).toBeLessThan(420);
	expect(filters!.x + filters!.width).toBeLessThanOrEqual(sort!.x);
	// With filters active the label grows ("Filters (1)"): the button still never overlaps the row.
	await go('/search?q=lantern+type:movie');
	await expect(row(page, 1)).toBeVisible();
	const wide = await page.getByTestId('facet-open').boundingBox();
	const sort2 = await page.getByLabel('Sort').boundingBox();
	expect(wide!.x + wide!.width).toBeLessThanOrEqual(sort2!.x);
	await expect(page.getByTestId('select-mode')).toBeHidden();
	// "Still growing" stays, as one line with its icon.
	const banner = page.locator('.rp-growing');
	expect((await banner.boundingBox())!.height).toBeLessThan(36);
	await expect(banner.locator('svg')).toBeVisible();
	const more = page
		.getByTestId('results-toolbar')
		.getByRole('button', { name: 'More', exact: true });
	await more.click();
	await expect(page.getByRole('button', { name: 'Save search' })).toBeVisible();
	await page.keyboard.press('Escape');
	await expect(page.getByRole('button', { name: 'Save search' })).toBeHidden();
	await expect(more).toBeFocused();
	await expectNoA11yViolations(page);
});

const compactCache = () =>
	localStorage.setItem('lodestar.appearance', JSON.stringify({ density: 'compact' }));

test('a Compact preference sets the bare link’s view, and typing keeps every key and runs one search', async ({
	page,
	mock
}) => {
	server(mock, () => body(hits(3), { total: 3 }));
	await page.addInitScript(compactCache);
	// `go` adds the project's density to the URL, which would name a view: load the bare link.
	await page.goto('/search');
	const editor = page.getByRole('combobox', { name: 'Search torrents' });
	await editor.click();
	await page.keyboard.type('lantern');
	await expect(editor).toHaveText('lantern');
	await expect(row(page, 1)).toHaveAttribute('data-density', 'compact');
	await expect(page.getByTestId('view-select')).toHaveValue('compact');
	await expect(page).not.toHaveURL(/density=/);
	await expect.poll(() => resultsCalls(mock).length).toBe(1);
	// One view pick is written, and a List stays a List though the preference is Compact.
	await page.getByTestId('view-select').selectOption('list');
	await expect(page).toHaveURL(/density=comfortable/);
	await expect(row(page, 1)).toHaveAttribute('data-density', 'comfortable');
	expect(resultsCalls(mock)).toHaveLength(1);
});

test('typing slowly into a bare link keeps every key under a Compact preference', async ({
	page,
	mock
}) => {
	server(mock, () => body(hits(3), { total: 3 }));
	await page.addInitScript(compactCache);
	await page.goto('/search');
	const editor = page.getByRole('combobox', { name: 'Search torrents' });
	await editor.click();
	await page.keyboard.type('lantern', { delay: 120 });
	await expect(editor).toHaveText('lantern');
	await expect(row(page, 1)).toHaveAttribute('data-density', 'compact');
	await expect(page).not.toHaveURL(/density=/);
	// Each key may have asked once; the settled query ran, and the last answer is the one shown.
	await expect.poll(() => resultsCalls(mock).at(-1)?.ast !== undefined).toBe(true);
});

for (const width of [390, 1440]) {
	test(`a preference the shell applies after the rows are up moves nothing at ${width} px`, async ({
		page,
		mock
	}) => {
		server(mock, () => body(hits(3), { total: 3 }));
		mock.on('searchIndexState', indexState(5_000_000)); // no banner, which a slow read can add
		await page.setViewportSize({ width, height: 900 });
		await page.addInitScript(() => {
			const w = window as unknown as { __cls: number };
			w.__cls = 0;
			new PerformanceObserver((list) => {
				for (const e of list.getEntries() as unknown as {
					value: number;
					hadRecentInput: boolean;
				}[])
					if (!e.hadRecentInput) w.__cls += e.value;
			}).observe({ type: 'layout-shift', buffered: true });
		});
		await page.goto('/search?q=lantern');
		await expect(row(page, 1)).toBeVisible();
		await expect(count(page)).toHaveText('3 results');
		const shift = () => page.evaluate(() => (window as unknown as { __cls: number }).__cls);
		const before = await shift();
		// The shell applies the saved preference with the first interaction, after the page starts.
		await page.evaluate(() => (document.documentElement.dataset.density = 'compact'));
		await expect(page.getByTestId('view-select')).toHaveValue('list');
		await expect(row(page, 1)).toHaveAttribute('data-density', 'comfortable');
		// Two frames, so a shift the change would cause has been reported.
		await page.evaluate(
			() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)))
		);
		expect(await shift()).toBe(before);
	});
}

test('a preference the shell applies late does not switch the rows when the drawer closes', async ({
	page,
	mock
}) => {
	server(mock, () => body(hits(3), { total: 3 }));
	await page.goto('/search?q=lantern');
	await expect(row(page, 2)).toBeVisible();
	await page.evaluate(() => (document.documentElement.dataset.density = 'compact'));
	await row(page, 2).click();
	await expect(page).toHaveURL(/\/t\//);
	await expect(page.getByTestId('drawer')).toBeVisible();
	await page.keyboard.press('Escape');
	await expect(page).toHaveURL(/\/search\?q=lantern/);
	await expect(rowOf(page, 2)).toBeFocused();
	await expect(row(page, 2)).toHaveAttribute('data-density', 'comfortable');
	await expect(page.getByTestId('view-select')).toHaveValue('list');
});

test.describe('the Filters button at 390 px never covers other content', () => {
	test.use({ viewport: { width: 390, height: 800 } });
	const overlaps = (a: { x: number; y: number; width: number; height: number }, b: typeof a) =>
		a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;

	test('the empty state’s title', async ({ page, mock }) => {
		server(mock, () => body([], { total: 0 }));
		await page.goto('/search?q=%20%20');
		const title = page.getByTestId('results-empty-title');
		await expect(title).toBeVisible();
		const filters = page.getByTestId('facet-open');
		if (await filters.count()) {
			expect(overlaps((await filters.boundingBox())!, (await title.boundingBox())!)).toBe(false);
		}
	});

	test('the “Details couldn’t load” alert above the toolbar', async ({ page, mock }) => {
		server(mock, () => body(hits(3), { total: 3 }));
		await page.goto('/search?q=lantern');
		await expect(row(page, 1)).toBeVisible();
		// What the page puts first in the results region when the drawer's chunk fails to load.
		await page.evaluate(() => {
			const alert = document.createElement('div');
			alert.setAttribute('role', 'alert');
			alert.textContent = 'Details couldn’t load.';
			alert.style.cssText = 'padding:24px;border:1px solid';
			document.getElementById('results')!.prepend(alert);
		});
		const alert = page.getByRole('alert').filter({ hasText: /Details couldn.t load/ });
		await expect(alert).toBeVisible();
		const filters = page.getByTestId('facet-open');
		expect(overlaps((await filters.boundingBox())!, (await alert.boundingBox())!)).toBe(false);
	});
});
