// Layout-shift counting for the specs that promise "no shift in this region". Call `watchShifts`
// before the page loads, then read `shifts` once the region has settled. Web fonts that swap in late
// move text by a pixel on any page, so a spec that counts shifts blocks `**/fonts/**` itself.
import type { Page } from '@playwright/test';

declare global {
	interface Window {
		__shifts?: number[];
	}
}

/** Records the value of every layout shift (not after user input) with a source inside `selector`. */
export async function watchShifts(page: Page, selector: string) {
	await page.route('**/fonts/**', (route) => route.abort());
	await page.addInitScript((sel) => {
		window.__shifts = [];
		new PerformanceObserver((list) => {
			for (const e of list.getEntries() as unknown as {
				value: number;
				hadRecentInput: boolean;
				sources: { node: Node | null }[];
			}[]) {
				if (e.hadRecentInput) continue;
				for (const s of e.sources) {
					const el = s.node instanceof Element ? s.node : s.node?.parentElement;
					if (el?.closest(sel)) window.__shifts!.push(e.value);
				}
			}
		}).observe({ type: 'layout-shift', buffered: true });
	}, selector);
}

export const shifts = (page: Page) => page.evaluate(() => window.__shifts ?? []);
