// TEMPORARY probe (to be deleted): logs every LCP candidate and the heading's width over time, to
// see why CI's Lighthouse measures a later LCP than the first paint.
import { liveEvents } from './support/events-server';
import { expect, test } from './support/test';

test.use({ viewport: { width: 412, height: 823 } });
test('probe: LCP candidates on /search', async ({ page, go }) => {
	await page.addInitScript(() => {
		const w = window as unknown as { __l: unknown[]; __w: unknown[] };
		w.__l = [];
		w.__w = [];
		new PerformanceObserver((list) => {
			for (const e of list.getEntries() as unknown as {
				startTime: number;
				size: number;
				element?: Element;
			}[])
				w.__l.push([Math.round(e.startTime), e.element?.tagName, e.element?.id, e.size]);
		}).observe({ type: 'largest-contentful-paint', buffered: true });
		let last = '';
		const tick = () => {
			const h = document.querySelector('h1');
			const r = h?.getBoundingClientRect();
			const s = `${h?.id}|${r?.width}|${r?.height}|${document.fonts.size}|${[...document.fonts].map((f) => f.status[0]).join('')}`;
			if (s !== last) {
				last = s;
				w.__w.push([Math.round(performance.now()), s]);
			}
			requestAnimationFrame(tick);
		};
		requestAnimationFrame(tick);
	});
	await liveEvents(page);
	await go('/search');
	await page.waitForTimeout(3000);
	const out = await page.evaluate(() => {
		const w = window as unknown as { __l: unknown; __w: unknown };
		return { lcp: w.__l, h1: w.__w, lang: navigator.language, ua: navigator.userAgent };
	});
	console.log('LCP-PROBE ' + JSON.stringify(out));
	expect(true).toBe(true);
});
