// Reduced motion (design-tokens §9, `motion-ui`): the project sets `reducedMotion: 'reduce'`, so
// every duration token is 0 except the opacity crossfade.
import { expect, test } from './support/test';

test('duration tokens collapse to 0ms under reduced motion', async ({ page, go }) => {
	await go('/search');
	expect(await page.evaluate(() => matchMedia('(prefers-reduced-motion: reduce)').matches)).toBe(
		true
	);
	// The build's CSS minifier may print 0ms as 0s, so compare milliseconds.
	const token = async (name: string) => {
		const raw = await page.evaluate(
			(n) => getComputedStyle(document.documentElement).getPropertyValue(n).trim(),
			name
		);
		const m = /^([\d.]+)(ms|s)$/.exec(raw);
		if (!m) throw new Error(`${name} is not a duration: "${raw}"`);
		return Number(m[1]) * (m[2] === 's' ? 1000 : 1);
	};
	for (const name of [
		'--duration-fast',
		'--duration-popover-out',
		'--duration-drawer-in',
		'--duration-drawer-out',
		'--duration-banner',
		'--duration-stagger'
	]) {
		expect(await token(name), name).toBe(0);
	}
	expect(await token('--duration-reduced-fade')).toBe(100);
});
