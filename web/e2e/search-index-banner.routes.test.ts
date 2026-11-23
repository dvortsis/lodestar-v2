// The index-state banner never moves painted rows (X-index-banner-late; §10.4 States): /search/state
// answered after the rows, at 390 and 1440 px, on a normal and a 4x slower CPU. Name ends in
// `routes.test.ts` so the `core` project runs it.
import type { Page } from '@playwright/test';
import { body, expect, hits, indexState, row, server } from './search-results.fixtures';
import { test } from './support/test';

type W = { __cls: number };
const observeShift = (page: Page) =>
	page.addInitScript(() => {
		const w = window as unknown as W;
		w.__cls = 0;
		new PerformanceObserver((list) => {
			for (const e of list.getEntries() as unknown as { value: number; hadRecentInput: boolean }[])
				if (!e.hadRecentInput) w.__cls += e.value;
		}).observe({ type: 'layout-shift', buffered: true });
	});
const shift = (page: Page) => page.evaluate(() => (window as unknown as W).__cls);

for (const cpu of [1, 4]) {
	for (const width of [390, 1440]) {
		test(`a late "still growing" never moves the rows at ${width} px, CPU ${cpu}x`, async ({
			page,
			go,
			mock
		}) => {
			await page.setViewportSize({ width, height: 900 });
			if (cpu > 1) {
				const cdp = await page.context().newCDPSession(page);
				await cdp.send('Emulation.setCPUThrottlingRate', { rate: cpu });
			}
			server(mock, () => body(hits(3), { total: 3 }));
			// /search/state answers only once the rows are on screen: past the panel's wait for it.
			let release!: () => void;
			const late = new Promise<void>((r) => (release = r));
			mock.on('searchIndexState', (async () => {
				await late;
				return indexState(84_210, { growing: true, growth_per_hour: 2104.5 });
			}) as never);
			await observeShift(page);
			await go('/search?q=lantern');
			await expect(row(page, 1)).toBeVisible({ timeout: 30_000 });
			const before = (await row(page, 1).boundingBox())!.y;
			release();
			await expect(page.getByTestId('results-cold-start')).toBeVisible({ timeout: 30_000 });
			// Two frames: the banner's layout has landed (and any shift it caused has been reported).
			await page.evaluate(
				() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)))
			);
			const after = (await row(page, 1).boundingBox())!.y;
			const cls = await shift(page);
			console.log(`width=${width} cpu=${cpu} before=${before} after=${after} cls=${cls}`);
			expect(after).toBe(before);
			expect(cls).toBeLessThan(1e-9);
		});
	}
}
