import { expect, type Page } from '@playwright/test';
import { fullPageAt } from './support/policy';

/**
 * Visual snapshot: waits for web fonts, freezes animations and the caret, and uses a small pixel
 * tolerance for sub-pixel anti-aliasing. Full page below 1920 px wide, viewport above. Baselines
 * are Linux-only and generated in CI's pinned container (see README).
 */
export async function expectPageSnapshot(page: Page, name: string) {
	await page.evaluate(() => document.fonts.ready);
	const width = page.viewportSize()?.width ?? 0;
	await expect(page).toHaveScreenshot(`${name}.png`, {
		fullPage: fullPageAt(width),
		animations: 'disabled',
		caret: 'hide',
		maxDiffPixelRatio: 0.001
	});
}
