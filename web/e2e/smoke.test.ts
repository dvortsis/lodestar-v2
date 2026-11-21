// Smoke test of every route, run by each matrix project (breakpoint × theme × density), the
// theme-sample projects and the reduced-motion project: it renders, applies the project's theme,
// has no axe violations and, for the routes the snapshot policy names, matches its baseline.
import { readdirSync, readFileSync } from 'node:fs';
import { expectNoA11yViolations } from './a11y';
import { snapshotName } from './support/policy';
import { ROUTES } from './support/routes';
import { expectPageSnapshot, freezeClock } from './snapshot';
import { expect, test } from './support/test';

for (const route of ROUTES) {
	test(`smoke ${route.name}`, async ({ page, go, mock, themeId, colorMode, density, shots }) => {
		await freezeClock(page);
		route.mock?.(mock);
		await go(route.path);
		await expect(page.locator('html')).toHaveAttribute('data-theme', themeId);
		await expect(page.locator('html')).toHaveCSS('color-scheme', colorMode);
		if (route.heading)
			await expect(page.getByRole('heading', { name: route.heading })).toBeVisible();
		await expectNoA11yViolations(page);
		const name = snapshotName(route, { shots, density, themeId });
		if (name) await expectPageSnapshot(page, name);
	});
}

// Every Settings screen the registry holds has a visible link in the navigation (a missing nav
// item once passed the 2560 px snapshots: the pixel tolerance, 0.1 % of the image, is 3,686 px
// there and a missing row moved 3,746 of them; docs/decisions/2026-11-21-X-snapshot-stale-outputs.md).
// The ids are read from the registry files themselves, the way screens.ts globs them, so a screen
// that is registered but missing from the navigation fails at every width, and a new screen needs no
// list edit. The registry has no visibility rule today: the navigation shows every screen (only the
// section filter narrows it). A screen that ever gets one must be named here, with its reason.
const FEATURES = new URL('../src/lib/features/', import.meta.url).pathname;
const registryFiles = [
	...readdirSync(FEATURES + 'settings', { withFileTypes: true })
		.filter((d) => d.isDirectory())
		.map((d) => `${FEATURES}settings/${d.name}/screen.ts`),
	...readdirSync(FEATURES, { withFileTypes: true })
		.filter((d) => d.isDirectory())
		.map((d) => `${FEATURES}${d.name}/screen.ts`)
].filter((file) => {
	try {
		readFileSync(file);
		return true;
	} catch {
		return false;
	}
});
const settingsScreenIds = registryFiles.flatMap((file) =>
	[...readFileSync(file, 'utf8').matchAll(/\bid:\s*'([a-z0-9-]+)',\s*group:/g)].map((m) => m[1])
);
const settingsRoute = ROUTES.find((r) => r.name === 'settings')!;

test('smoke settings navigation: every registered screen has a link', async ({
	page,
	go,
	mock
}) => {
	// The registry is read from disk: an empty list would pass for the wrong reason.
	expect(settingsScreenIds.length).toBeGreaterThanOrEqual(22);
	expect(new Set(settingsScreenIds).size).toBe(settingsScreenIds.length);
	await freezeClock(page);
	settingsRoute.mock?.(mock);
	// The overview shows the navigation at every width; a section's page hides it below 1024 px.
	await go('/settings');
	const nav = page.getByRole('navigation', { name: 'Settings sections' });
	await expect(nav).toBeVisible();
	for (const id of settingsScreenIds)
		await expect(
			nav.locator(`a[href*="/settings/${id}"]`),
			`the ${id} screen's link`
		).toBeVisible();
	// And no link without a screen.
	await expect(nav.getByRole('link')).toHaveCount(settingsScreenIds.length);
});
