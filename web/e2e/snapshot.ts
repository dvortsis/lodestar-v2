import { expect, type Page } from '@playwright/test';

/**
 * Full-page visual snapshot: waits for web fonts, freezes animations and the caret, and uses a
 * small pixel tolerance for sub-pixel anti-aliasing. Baselines are Linux-only (see README).
 */
export async function expectPageSnapshot(page: Page, name: string) {
	await page.evaluate(() => document.fonts.ready);
	await expect(page).toHaveScreenshot(`${name}.png`, {
		fullPage: true,
		animations: 'disabled',
		caret: 'hide',
		maxDiffPixelRatio: 0.001
	});
}
