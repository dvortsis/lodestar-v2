// The "still growing" banner never moves painted rows (X-index-banner-late; §10.4 States): with
// /search/state answered only after the rows are on screen, at 390 and 1440 px on a normal and a 4x
// slower CPU. Name ends in `routes.test.ts` so the `core` project runs it.
import type { Page } from '@playwright/test';
import { body, expect, hits, indexState, row, server } from './search-results.fixtures';
import { test } from './support/test';

type W = { __cls: number; __mine: number };
/**
 * Layout shifts: `__cls` counts every shift since the last `resetShift`, `__mine` those whose moved
 * node is in the results region (`#results`) since the page opened. A boot-time nudge of the nav
 * links (sub-1e-4, 4x CPU at 1440 px, before any row) is outside both the region and the window.
 */
const observeShift = (page: Page) =>
	page.addInitScript(() => {
		const w = window as unknown as W;
		w.__cls = 0;
		w.__mine = 0;
		new PerformanceObserver((list) => {
			for (const e of list.getEntries() as unknown as {
				value: number;
				hadRecentInput: boolean;
				sources?: { node?: Node | null }[];
			}[]) {
				if (e.hadRecentInput) continue;
				w.__cls += e.value;
				const own = (e.sources ?? []).some((x) => {
					const n = x.node?.nodeType === 3 ? x.node.parentElement : (x.node as Element | null);
					return !!n?.closest?.('#results');
				});
				if (own) w.__mine += e.value;
			}
		}).observe({ type: 'layout-shift', buffered: true });
	});
const resetShift = (page: Page) => page.evaluate(() => void ((window as unknown as W).__cls = 0));
const shift = (page: Page) =>
	page.evaluate(() => ({
		cls: (window as unknown as W).__cls,
		mine: (window as unknown as W).__mine
	}));
/** Two frames: a layout that just changed has landed, and any shift it caused is reported. */
const frames = (page: Page) =>
	page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
/** What the browser kept from the last /search/state (`index-memory.ts`). */
const kept = (page: Page) => page.evaluate(() => localStorage.getItem('lodestar.indexState'));

/** /search/state answers only when `release` runs: after the rows, past the panel's wait for it. */
function lateState(mock: Parameters<typeof server>[0], searchable: number) {
	let release!: () => void;
	const late = new Promise<void>((r) => (release = r));
	mock.on('searchIndexState', (async () => {
		await late;
		return indexState(searchable, { growing: true, growth_per_hour: 2104.5 });
	}) as never);
	return () => release();
}

async function setUp(page: Page, width: number, cpu: number) {
	await page.setViewportSize({ width, height: 900 });
	if (cpu > 1) {
		const cdp = await page.context().newCDPSession(page);
		await cdp.send('Emulation.setCPUThrottlingRate', { rate: cpu });
	}
	await observeShift(page);
}

for (const cpu of [1, 4]) {
	for (const width of [390, 1440]) {
		test(`a late "still growing" never moves the rows at ${width} px, CPU ${cpu}x`, async ({
			page,
			go,
			mock
		}) => {
			await setUp(page, width, cpu);
			// An earlier visit saw a growing index of 80,000.
			await page.addInitScript(() =>
				localStorage.setItem(
					'lodestar.indexState',
					JSON.stringify({
						v: 1,
						growing: true,
						searchable_torrents: 80_000,
						growth_per_hour: 2000
					})
				)
			);
			server(mock, () => body(hits(3), { total: 3 }));
			const release = lateState(mock, 84_210);
			await go('/search?q=lantern');
			await expect(row(page, 1)).toBeVisible({ timeout: 30_000 });
			// The remembered banner is there with the first rows.
			const line = page.getByTestId('results-cold-start');
			await expect(line).toContainText('80,000');
			const before = (await row(page, 1).boundingBox())!.y;
			const tall = (await page.locator('.rp-growing').boundingBox())!.height;
			await resetShift(page);
			release();
			// The live figures fill in place: same line height, the rows stay put.
			await expect(line).toContainText('84,210', { timeout: 30_000 });
			await frames(page);
			expect((await page.locator('.rp-growing').boundingBox())!.height).toBe(tall);
			expect((await row(page, 1).boundingBox())!.y).toBe(before);
			const { cls, mine } = await shift(page);
			expect(mine).toBe(0);
			expect(cls).toBeLessThan(1e-9);
			expect(JSON.parse((await kept(page))!)).toMatchObject({ searchable_torrents: 84_210 });
		});

		test(`a first visit holds a late "still growing" until no rows show at ${width} px, CPU ${cpu}x`, async ({
			page,
			go,
			mock
		}) => {
			await setUp(page, width, cpu);
			let open!: () => void;
			const gate = new Promise<void>((r) => (open = r));
			server(mock, async (sent) => {
				const text = JSON.stringify(sent.ast);
				if (text.includes('nothing')) return body([]);
				if (text.includes('slow')) await gate;
				return body(hits(3), { total: 3 });
			});
			const release = lateState(mock, 84_210);
			await go('/search?q=lantern');
			await expect(row(page, 1)).toBeVisible({ timeout: 30_000 });
			const before = (await row(page, 1).boundingBox())!.y;
			await resetShift(page);
			release();
			// The answer has landed (the browser keeps it), but the rows are on screen: no banner yet.
			await expect.poll(() => kept(page), { timeout: 30_000 }).not.toBeNull();
			await frames(page);
			await expect(page.getByTestId('results-cold-start')).toHaveCount(0);
			expect((await row(page, 1).boundingBox())!.y).toBe(before);
			const { cls, mine } = await shift(page);
			expect(mine).toBe(0);
			expect(cls).toBeLessThan(1e-9);
			// A query with no matches: no rows, so the banner shows now.
			const bar = page.getByRole('combobox', { name: 'Search torrents' });
			await bar.click();
			await page.keyboard.press('ControlOrMeta+a');
			await page.keyboard.type('nothing');
			await page.keyboard.press('Enter');
			await expect(page.getByTestId('results-none')).toBeVisible({ timeout: 30_000 });
			await expect(page.getByTestId('results-cold-start')).toContainText('84,210');
			// And the next query's first load keeps it above its skeletons, where its rows will land.
			await bar.click();
			await page.keyboard.press('ControlOrMeta+a');
			await page.keyboard.type('slow');
			await page.keyboard.press('Enter');
			await expect(page.getByTestId('results-loading')).toBeVisible({ timeout: 30_000 });
			await expect(page.getByTestId('results-cold-start')).toBeVisible();
			open();
			await expect(row(page, 1)).toBeVisible({ timeout: 30_000 });
			await expect(page.getByTestId('results-cold-start')).toBeVisible();
		});
	}
}
