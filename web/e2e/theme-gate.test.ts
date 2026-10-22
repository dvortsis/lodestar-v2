// Theme gate: the pre-paint script (app.html) must set the stored theme before first paint.
// A probe injected ahead of the page records the root background at DOMContentLoaded and in the
// first animation frame; both must already be the stored dark theme's colour, never the light
// default (no flash of the wrong theme).
import { expect, test, type Page } from '@playwright/test';
import { ALL_THEMES, hexToRgb, riverstoneToken } from './support/themes';
import { applyTheme } from './support/test';

async function probe(page: Page) {
	await page.addInitScript(() => {
		const w = window as unknown as { __frames: Record<string, string> };
		w.__frames = {};
		const snap = (when: string) => {
			const root = document.documentElement;
			w.__frames[when] = `${root.dataset.theme}|${getComputedStyle(root).backgroundColor}`;
		};
		// Registered at document start, so this is the first frame the page can produce.
		requestAnimationFrame(() => snap('first-frame'));
		document.addEventListener('DOMContentLoaded', () => snap('domcontentloaded'));
	});
}

async function frames(page: Page) {
	await page.waitForFunction(
		() => 'first-frame' in (window as never as { __frames: object }).__frames
	);
	return page.evaluate(() => (window as never as { __frames: Record<string, string> }).__frames);
}

test('stored Riverstone dark paints dark on the first frame', async ({ page }) => {
	await probe(page);
	await applyTheme(page, 'riverstone-dark');
	// The OS says light: the stored mode must win over the media query.
	await page.emulateMedia({ colorScheme: 'light' });
	await page.goto('/search');
	const expected = `riverstone-dark|${hexToRgb(riverstoneToken('dark', '--color-base-100'))}`;
	expect(await frames(page)).toEqual({ domcontentloaded: expected, 'first-frame': expected });
});

test('a stored non-Riverstone dark theme paints its own background on the first frame', async ({
	page
}) => {
	const theme = ALL_THEMES.find((t) => t.id === 'nord-dark')!;
	await probe(page);
	await applyTheme(page, theme.id);
	await page.emulateMedia({ colorScheme: 'light' });
	await page.goto('/search');
	const expected = `nord-dark|${hexToRgb(theme.vars!['--color-base-100'])}`;
	expect(await frames(page)).toEqual({ domcontentloaded: expected, 'first-frame': expected });
});

test('without storage the OS colour scheme picks Riverstone light or dark', async ({ page }) => {
	for (const scheme of ['light', 'dark'] as const) {
		await page.emulateMedia({ colorScheme: scheme });
		await page.goto('/search');
		await expect(page.locator('html')).toHaveAttribute('data-theme', `riverstone-${scheme}`);
	}
});
