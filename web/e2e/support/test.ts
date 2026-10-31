// The test object for harness-backed specs. It extends Playwright's with:
//   - appearance options set per project (`themeId`, `colorMode`, `density`), applied through the
//     app's own mechanisms: the `lodestar.theme` storage key read by the pre-paint script, and the
//     `density` URL parameter of §10.3 (see `go`);
//   - `mock`: the OpenAPI mock, installed for every test (auto) so unmocked API calls fail loudly;
//   - `kbd`: the keyboard-only guard.
import { test as base, expect, type Page } from '@playwright/test';
import {
	discoverFixture,
	fetchFixture,
	indexFixture,
	serveFixture,
	understandFixture
} from '../../src/lib/features/insights/stages/fixtures';
import { OpenApiMock } from '../mock/openapi-mock';
import { KeyboardOnly } from './keyboard';
import type { Density } from './matrix';
import { ALL_THEMES, type Mode } from './themes';

export interface Appearance {
	/** `riverstone-light`, `nord-dark`, … */
	themeId: string;
	colorMode: Mode;
	density: Density;
	/** `reduce` emulates prefers-reduced-motion. Set with `contextOptions.reducedMotion` too (see config). */
	motion: 'reduce' | null;
	/**
	 * Which baselines the project owns: `matrix` (key screens per breakpoint/theme/density),
	 * `themes` (key screens in the theme set, at 1366), or `none` (axe and render checks only).
	 */
	shots: 'matrix' | 'themes' | 'none';
}

/** Writes `lodestar.theme` before any page script runs, exactly as the Appearance settings do. */
export async function applyTheme(
	page: Page,
	themeId: string,
	reducedMotion: 'reduce' | 'no-preference' | null = null
) {
	const theme = ALL_THEMES.find((t) => t.id === themeId);
	if (!theme) throw new Error(`unknown theme "${themeId}"`);
	const stored: Record<string, unknown> = { mode: theme.mode };
	if (theme.vars) stored[theme.mode] = { id: theme.id, vars: theme.vars };
	await page.addInitScript((value) => {
		try {
			localStorage.setItem('lodestar.theme', JSON.stringify(value));
		} catch {
			/* storage blocked: the app falls back to Riverstone, the test will say so */
		}
	}, stored);
	// emulateMedia replaces the whole emulation, so carry the project's reduced-motion setting.
	await page.emulateMedia({ colorScheme: theme.mode, reducedMotion });
}

export const test = base.extend<
	Appearance & { mock: OpenApiMock; kbd: KeyboardOnly; go: (path: string) => Promise<void> }
>({
	themeId: ['riverstone-light', { option: true }],
	colorMode: ['light', { option: true }],
	density: ['comfortable', { option: true }],
	motion: [null, { option: true }],
	shots: ['none', { option: true }],

	page: async ({ page, themeId, motion }, use) => {
		await applyTheme(page, themeId, motion);
		await use(page);
	},

	mock: [
		async ({ page }, use) => {
			const mock = new OpenApiMock();
			await mock.install(page);
			// Every overview drill-through lands on a stage page; a spec overrides these.
			mock.on('getInsightsDiscover', discoverFixture());
			mock.on('getInsightsFetch', fetchFixture());
			mock.on('getInsightsUnderstand', understandFixture());
			mock.on('getInsightsIndex', indexFixture());
			mock.on('getInsightsServe', serveFixture());
			await use(mock);
			mock.assertNoFailures();
		},
		{ auto: true }
	],

	kbd: async ({ page }, use) => {
		const kbd = new KeyboardOnly(page);
		await kbd.install();
		await use(kbd);
		expect(await kbd.pointerEvents(), 'pointer used in a keyboard-only test').toEqual([]);
	},

	/** `page.goto` with the project's density in the URL (§10.3: `?density=`). */
	go: async ({ page, density }, use) => {
		await use(async (path) => {
			const url = new URL(path, 'http://x');
			url.searchParams.set('density', density);
			await page.goto(url.pathname + url.search);
		});
	}
});

export { expect };
