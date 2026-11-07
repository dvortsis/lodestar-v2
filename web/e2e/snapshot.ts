import { expect, type Page } from '@playwright/test';
import { NOW } from '../src/lib/features/insights/overview/fixtures';
import { fullPageAt } from './support/policy';

/**
 * Pins the page's `Date` to the fixtures' "now" (2026-10-23 12:00 UTC); timers still run. Call it
 * before the page loads in any spec that takes a snapshot: relative times ("Updated just now",
 * "Seen 2 years ago") are computed from the clock, so with the real one a baseline goes stale as
 * the calendar moves (on 2026-11-06 the Insights bar went from "last week" to "2 weeks ago").
 */
export async function freezeClock(page: Page) {
	await page.clock.setFixedTime(NOW * 1000);
}

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
