// Smoke test of every route, run by each matrix project (breakpoint × theme × density), the
// theme-sample projects and the reduced-motion project: it renders, applies the project's theme,
// has no axe violations and, for the routes the snapshot policy names, matches its baseline.
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { expectNoA11yViolations } from './a11y';
import { snapshotName } from './support/policy';
import { ROUTES } from './support/routes';
import { expectPageSnapshot, freezeClock } from './snapshot';
import { settle } from './support/settle';
import { expect, test } from './support/test';

for (const route of ROUTES) {
	test(`smoke ${route.name}`, async ({ page, go, mock, themeId, colorMode, density, shots }) => {
		await freezeClock(page);
		route.mock?.(mock);
		await go(route.path);
		await expect(page.locator('html')).toHaveAttribute('data-theme', themeId);
		await expect(page.locator('html')).toHaveCSS('color-scheme', colorMode);
		// Settled before axe and the shot: a still skeleton passes the snapshot's stability check.
		await settle(page, route);
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
const FEATURES = fileURLToPath(new URL('../src/lib/features/', import.meta.url));
const registryFiles = [
	...readdirSync(join(FEATURES, 'settings'), { withFileTypes: true })
		.filter((d) => d.isDirectory())
		.map((d) => join(FEATURES, 'settings', d.name, 'screen.ts')),
	...readdirSync(FEATURES, { withFileTypes: true })
		.filter((d) => d.isDirectory())
		.map((d) => join(FEATURES, d.name, 'screen.ts'))
].filter((file) => existsSync(file));
// Every `id:` in these files is a screen's id, whatever the order of its keys or its quoting; a
// file whose `id:` count differs from what was read (an id that is a constant, say) fails loudly
// instead of shrinking the list.
const settingsScreenIds = registryFiles.flatMap((file) => {
	const text = readFileSync(file, 'utf8');
	const ids = [...text.matchAll(/\bid:\s*['"]([^'"]+)['"]/g)].map((m) => m[1]);
	if (ids.length !== (text.match(/\bid:/g) ?? []).length)
		throw new Error(`${file}: an id the smoke test can't read; write it as a string literal`);
	return ids;
});
const settingsRoute = ROUTES.find((r) => r.name === 'settings')!;

test('smoke settings navigation: every registered screen has a link', async ({
	page,
	go,
	mock,
	shots
}) => {
	// Theme, density and motion can't change which links exist: the matrix's widths are the point.
	test.skip(shots !== 'matrix', 'the navigation does not depend on the theme or motion');
	// The registry is read from disk: an empty list would pass for the wrong reason (a retired
	// screen needs no edit here; the floor only guards an empty read).
	expect(settingsScreenIds.length).toBeGreaterThanOrEqual(10);
	expect(new Set(settingsScreenIds).size).toBe(settingsScreenIds.length);
	await freezeClock(page);
	settingsRoute.mock?.(mock);
	// The overview shows the navigation at every width; a section's page hides it below 1024 px.
	await go('/settings');
	const nav = page.getByRole('navigation', { name: 'Settings sections' });
	await expect(nav).toBeVisible();
	for (const id of settingsScreenIds)
		await expect(
			nav.locator(`a[href="/settings/${id}"], a[href^="/settings/${id}?"]`),
			`the ${id} screen's link`
		).toBeVisible();
	// And no link without a screen.
	await expect(nav.getByRole('link')).toHaveCount(settingsScreenIds.length);
});
