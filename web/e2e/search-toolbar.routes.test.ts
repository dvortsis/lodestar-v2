// S-search-3 against the OpenAPI mock: the results toolbar (count, sort, density, Save search),
// the "Load more" fallback and end line under the list, no layout shift on a pasted link, and axe.
import type { Page } from '@playwright/test';
import { expectNoA11yViolations } from './a11y';
import {
	body,
	expect,
	hits,
	indexState,
	toAst,
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

test('"N hidden · Show" counts what a filter rule hid, and Show adds them (P7)', async ({
	page,
	go,
	mock
}) => {
	server(mock, (sent) =>
		(sent as { hidden?: boolean }).hidden
			? body(hits(5), { total: 5 })
			: body(hits(3), { total: 3 })
	);
	// Asked after the rows, with the rows' query; held a moment so the count shows alone first.
	let release!: () => void;
	const held = new Promise<void>((r) => (release = r));
	mock.on('searchHiddenCount', async () => (await held, { hidden_total: 2 }));
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	await expect(count(page)).toHaveText('3 results');
	const box = await count(page).boundingBox();
	release();
	await expect(count(page)).toHaveText('3 results · 2 hidden by filters · Show', {
		useInnerText: true
	});
	// The line keeps its height: nothing under it moves.
	expect((await count(page).boundingBox())?.height).toBe(box?.height);
	const sent = mock.calls('searchHiddenCount').map((r) => r.postDataJSON());
	expect(sent).toHaveLength(1);
	expect(sent[0]).toMatchObject({ hidden: false, ast: toAst('lantern') });
	const show = page.getByRole('button', { name: 'Show', exact: true });
	await expect(show).toHaveAccessibleDescription('2 hidden by filters');
	await show.click();
	await expect.poll(() => new URL(page.url()).searchParams.get('hidden')).toBe('1');
	await expect(count(page)).toHaveText('5 results');
	// Focus goes to the results it opened, not the page.
	await expect(page.locator('#results')).toBeFocused();
	await expect(page.getByTestId('results-hidden-show')).toHaveCount(0);
	await expectNoA11yViolations(page);
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
			(w as unknown as { __mine: number }).__mine = 0;
			new PerformanceObserver((list) => {
				for (const e of list.getEntries() as unknown as {
					value: number;
					hadRecentInput: boolean;
					sources?: { node?: Node | null }[];
				}[]) {
					if (e.hadRecentInput) continue;
					w.__cls += e.value;
					// Attribution: a shift whose moved node is the Filters host, the toolbar or anything in
					// the results region is this slice's, at any size.
					const own = (e.sources ?? []).some((x) => {
						const n = x.node?.nodeType === 3 ? x.node.parentElement : (x.node as Element | null);
						return !!n?.closest?.('#search-rail, #results');
					});
					if (own) (w as unknown as { __mine: number }).__mine += e.value;
				}
			}).observe({ type: 'layout-shift', buffered: true });
		});
		await go('/search?q=lantern');
		await expect(count(page)).toHaveText('3 results');
		await expect(row(page, 1)).toBeVisible();
		const { cls, mine } = await page.evaluate(() => ({
			cls: (window as unknown as { __cls: number }).__cls,
			mine: (window as unknown as { __mine: number }).__mine
		}));
		// A shift of the Filters host, the toolbar or the results region fails at any size. What is
		// left (CI's fonts leave a sub-pixel 1e-5 here and there, outside those elements) is bounded
		// far under the page's 0.02 budget.
		expect(mine).toBe(0);
		expect(cls).toBeLessThan(0.001);
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
			(w as unknown as { __mine: number }).__mine = 0;
			new PerformanceObserver((list) => {
				for (const e of list.getEntries() as unknown as {
					value: number;
					hadRecentInput: boolean;
					sources?: { node?: Node | null }[];
				}[]) {
					if (e.hadRecentInput) continue;
					w.__cls += e.value;
					// Attribution: a shift whose moved node is the Filters host, the toolbar or anything in
					// the results region is this slice's, at any size.
					const own = (e.sources ?? []).some((x) => {
						const n = x.node?.nodeType === 3 ? x.node.parentElement : (x.node as Element | null);
						return !!n?.closest?.('#search-rail, #results');
					});
					if (own) (w as unknown as { __mine: number }).__mine += e.value;
				}
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
		const { cls, mine } = await page.evaluate(() => ({
			cls: (window as unknown as { __cls: number }).__cls,
			mine: (window as unknown as { __mine: number }).__mine
		}));
		// A shift of the Filters host, the toolbar or the results region fails at any size. What is
		// left (CI's fonts leave a sub-pixel 1e-5 here and there, outside those elements) is bounded
		// far under the page's 0.02 budget.
		expect(mine).toBe(0);
		expect(cls).toBeLessThan(0.001);
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
			(w as unknown as { __mine: number }).__mine = 0;
			new PerformanceObserver((list) => {
				for (const e of list.getEntries() as unknown as {
					value: number;
					hadRecentInput: boolean;
					sources?: { node?: Node | null }[];
				}[]) {
					if (e.hadRecentInput) continue;
					w.__cls += e.value;
					// Attribution: a shift whose moved node is the Filters host, the toolbar or anything in
					// the results region is this slice's, at any size.
					const own = (e.sources ?? []).some((x) => {
						const n = x.node?.nodeType === 3 ? x.node.parentElement : (x.node as Element | null);
						return !!n?.closest?.('#search-rail, #results');
					});
					if (own) (w as unknown as { __mine: number }).__mine += e.value;
				}
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

	test('the home screen has no Filters button to cover its title', async ({ page, mock }) => {
		server(mock, () => body([], { total: 0 }));
		await page.goto('/search?q=%20%20');
		await expect(page.getByTestId('results-empty-title')).toBeVisible();
		// X-shell-home: the home has no facets, so the button is hidden with the rail.
		await expect(page.getByTestId('facet-open')).toBeHidden();
	});

	test('with no results for a real query, the Filters button does not cover the empty state', async ({
		page,
		mock
	}) => {
		server(mock, () => body([], { total: 0 }));
		await page.goto('/search?q=nothingmatchesthis');
		const none = page.getByText('No torrents match').first();
		await expect(none).toBeVisible();
		// For a real query the facets are there, so the button shows (the home has none).
		const filters = page.getByTestId('facet-open');
		await expect(filters).toBeVisible();
		expect(overlaps((await filters.boundingBox())!, (await none.boundingBox())!)).toBe(false);
	});

	test('on a results page the Filters button does not cover the count', async ({ page, mock }) => {
		server(mock, () => body(hits(3), { total: 3 }));
		await page.goto('/search?q=lantern');
		await expect(row(page, 1)).toBeVisible();
		const count = page.getByTestId('results-count');
		const filters = page.getByTestId('facet-open');
		await expect(count).toBeVisible();
		await expect(filters).toBeVisible();
		expect(overlaps((await filters.boundingBox())!, (await count.boundingBox())!)).toBe(false);
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

test.describe('"N hidden · Show" under a coarse pointer', () => {
	test.use({ hasTouch: true, isMobile: true, viewport: { width: 390, height: 800 } });

	test('Show is a 44 px target and its arrival leaves the count line as it was', async ({
		page,
		go,
		mock
	}) => {
		server(mock, () => body(hits(3), { total: 3 }));
		let release!: () => void;
		const held = new Promise<void>((r) => (release = r));
		mock.on('searchHiddenCount', async () => (await held, { hidden_total: 2 }));
		await go('/search?q=lantern');
		await expect(row(page, 1)).toBeVisible();
		await expect(count(page)).toHaveText('3 results');
		const before = await count(page).boundingBox();
		const first = await row(page, 1).boundingBox();
		release();
		const show = page.getByTestId('results-hidden-show');
		await expect(show).toBeVisible();
		const after = await count(page).boundingBox();
		expect(after?.height).toBe(before?.height);
		expect((await row(page, 1).boundingBox())?.y).toBe(first?.y);
		const target = await show.evaluate((el) => {
			const r = el.getBoundingClientRect();
			const s = getComputedStyle(el);
			const pad = (a: string, b: string) => parseFloat(s[a as never]) + parseFloat(s[b as never]);
			return { w: r.width, h: r.height, py: pad('paddingTop', 'paddingBottom') };
		});
		// Padding taken back by a negative margin: the box is at least 44 px, the line unchanged.
		expect(target.w).toBeGreaterThanOrEqual(44);
		expect(target.h).toBeGreaterThanOrEqual(44);
		expect(target.py).toBeGreaterThan(0);
	});
});

// X-hidden-count (runner): the note arrives after the rows and moves nothing. Where even "N filtered
// · Show" would wrap the count line or the controls it shows no note ("Show hidden" stays in
// Filters). From 128rem (2,048 px) the controls follow the count (L3), so they move sideways when it
// arrives, so the count's slot has a floor (toolbar.css) and nothing moves at any width.
for (const [width, shows] of [
	[360, false],
	[390, true],
	[768, false],
	[900, true],
	[1024, true],
	[1366, true],
	[2048, true],
	[2560, true]
] as const) {
	test(`at ${width} px the hidden count ${shows ? 'shows' : 'stays out'} and nothing moves`, async ({
		page,
		go,
		mock
	}) => {
		await page.setViewportSize({ width, height: 900 });
		server(mock, () => body(hits(3), { total: 2196 }));
		let release!: () => void;
		const held = new Promise<void>((r) => (release = r));
		mock.on('searchHiddenCount', async () => (await held, { hidden_total: 12 }));
		await go('/search?q=lantern');
		await expect(row(page, 1)).toBeVisible();
		const toolbar = page.getByTestId('results-toolbar');
		const before = await toolbar.boundingBox();
		const first = (await row(page, 1).boundingBox())?.y;
		const sort = await page.getByLabel('Sort').boundingBox();
		release();
		const show = page.getByTestId('results-hidden-show');
		await expect(show).toHaveCount(1);
		if (shows) await expect(show).toBeVisible();
		else await expect(show).toBeHidden();
		expect((await toolbar.boundingBox())?.height).toBe(before?.height);
		expect((await row(page, 1).boundingBox())?.y).toBe(first);
		const after = await page.getByLabel('Sort').boundingBox();
		expect(after?.y).toBe(sort?.y);
		expect(after?.x).toBe(sort?.x);
	});
}

// The worst case for the count's reserved slot (toolbar.css, from 128rem): an approximate total
// plus a 7-digit hidden count with the full words, in the two shipped languages.
for (const lang of ['en', 'zh-CN'] as const) {
	test.describe(`the longest count in ${lang}`, () => {
		test.use({ locale: lang });
		test('at 2,048 px it and its note move nothing', async ({ page, go, mock }) => {
			await page.setViewportSize({ width: 2048, height: 900 });
			server(mock, () => body(hits(3), { total: 2_400_000, total_approx: true }));
			let release!: () => void;
			const held = new Promise<void>((r) => (release = r));
			mock.on('searchHiddenCount', async () => (await held, { hidden_total: 9_999_999 }));
			await go('/search?q=lantern');
			await expect(row(page, 1)).toBeVisible();
			await expect(count(page)).toContainText('≈');
			const toolbar = page.getByTestId('results-toolbar');
			const before = await toolbar.boundingBox();
			const first = (await row(page, 1).boundingBox())?.y;
			const sort = await page.getByLabel(/^(Sort|排序)/).boundingBox();
			release();
			await expect(page.getByTestId('results-hidden-show')).toBeVisible();
			const text = await count(page).evaluate((el) => {
				const r = document.createRange();
				r.selectNodeContents(el);
				return r.getBoundingClientRect().width;
			});
			// The floor in toolbar.css (384 px) must hold the text: a font change says so here.
			expect(text).toBeLessThanOrEqual(384);
			expect((await toolbar.boundingBox())?.height).toBe(before?.height);
			expect((await row(page, 1).boundingBox())?.y).toBe(first);
			expect(await page.getByLabel(/^(Sort|排序)/).boundingBox()).toEqual(sort);
		});
	});
}

test('below 46rem of toolbar Sort and View keep their names, without visible labels', async ({
	page,
	go,
	mock
}) => {
	await page.setViewportSize({ width: 1024, height: 900 });
	server(mock, () => body(hits(3)));
	await go('/search?q=lantern');
	await expect(row(page, 1)).toBeVisible();
	for (const name of ['Sort', 'View']) {
		await expect(page.getByRole('combobox', { name, exact: true })).toBeVisible();
		const label = page.locator('.tb-lbl > span', { hasText: name }).first();
		expect((await label.boundingBox())?.width).toBeLessThanOrEqual(1);
	}
});
