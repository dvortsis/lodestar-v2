// Screenshot helper for the docs project: freezes the clock, outlines the element a step refers
// to (the focus-ring tokens, no hex), takes the shot and writes an optimised PNG.
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, type Locator, type Page } from '@playwright/test';
import sharp from 'sharp';

/** Where the images go: `docs/user/images/` at the repository root (cwd is `web/`). */
export const OUT =
	process.env.DOCS_SHOTS_DIR ?? join(process.cwd(), '..', 'docs', 'user', 'images');
/** Widest image written; shots are taken at 1366 so this only caps outliers. */
const MAX_WIDTH = 1366;

/** The clock stops at a fixed instant, so relative times and "updated" stamps never change. */
export async function freeze(page: Page) {
	await page.clock.install({ time: new Date('2026-01-15T12:00:00Z') });
	await page.clock.pauseAt(new Date('2026-01-15T12:00:05Z'));
}

const OUTLINE = 'data-docs-outline';

/** Waits (to a deadline, no fixed sleep) until fonts are in and the page height stops changing. */
async function settle(page: Page) {
	await page.evaluate(() => document.fonts.ready);
	let last = -1;
	let same = 0;
	await expect
		.poll(
			async () => {
				const h = await page.evaluate(() => document.documentElement.scrollHeight);
				same = h === last ? same + 1 : 0;
				last = h;
				return same;
			},
			{ timeout: 10_000, intervals: [100] }
		)
		.toBeGreaterThanOrEqual(5);
}

/** `fullPage: false` keeps only the first screen (long overview pages). */
export async function capture(
	page: Page,
	name: string,
	outline?: Locator,
	opts: { fullPage?: boolean } = {}
) {
	await settle(page);
	await outlineOf(page, outline);
	const png = await page.screenshot({
		fullPage: opts.fullPage ?? true,
		animations: 'disabled',
		caret: 'hide'
	});
	// Palette PNGs of UI flat colour are small and byte-stable across runs.
	const out = await sharp(png)
		.resize({ width: MAX_WIDTH, withoutEnlargement: true })
		.png({ palette: true, quality: 90, effort: 10, compressionLevel: 9 })
		.toBuffer();
	mkdirSync(OUT, { recursive: true });
	writeFileSync(join(OUT, `${name}.png`), out);
}

async function outlineOf(page: Page, target?: Locator) {
	if (!target) return;
	await target.evaluate((el, attr) => el.setAttribute(attr, ''), OUTLINE);
	await page.addStyleTag({
		content: `[${OUTLINE}] { outline: calc(var(--focus-ring-width) + 1px) solid var(--color-primary) !important; outline-offset: var(--focus-ring-offset) !important; }`
	});
}
