import { defineConfig, type Project } from '@playwright/test';
import type { Appearance } from './e2e/support/test';
import { BREAKPOINTS, DENSITIES, DESKTOP } from './e2e/support/matrix';
import { RIVERSTONE, rotationSeed, sampleThemes } from './e2e/support/themes';

// Projects (filter with `--project=<name>`; see e2e/README.md):
//   core                          pre-existing specs + harness self-tests + theme gate
//   bp<width>-<theme>-<density>   smoke per breakpoint × Riverstone light/dark × density: axe on every
//                                 route, baselines for key routes (Compact: density-aware routes)
//   sample-<theme-id>             smoke at 1366 for the rotating 6 non-Riverstone themes; key-route
//                                 baselines come from the shared 46-theme set
//   reduced-motion                axe + motion tokens with prefers-reduced-motion: reduce (no baselines)
//   keyboard-only                 *.keyboard.test.ts flows (no pointer allowed)
//   all-themes                    axe on every route in all 48 themes + the 46-theme key-route
//                                 baselines (nightly; opt-in: --project=all-themes or E2E_ALL_THEMES=1)

const allThemes = !!process.env.E2E_ALL_THEMES || process.argv.join(' ').includes('all-themes');
const smoke = /smoke\.test\.ts$/;
// The theme set's baselines (key screens × 46 non-Riverstone themes, at 1366) are shared by the
// rotating `sample-*` projects and the nightly `all-themes` project: no project name in the path.
const themeSnapshots = '{snapshotDir}/{testFileDir}/theme-snapshots/{arg}{-snapshotSuffix}{ext}';
const desktop = { viewport: { width: DESKTOP.width, height: DESKTOP.height } };

const matrix: Project<Appearance>[] = BREAKPOINTS.flatMap((bp) =>
	RIVERSTONE.flatMap((theme) =>
		DENSITIES.map((density) => ({
			name: `bp${bp.name}-${theme.id}-${density}`,
			testMatch: smoke,
			use: {
				viewport: { width: bp.width, height: bp.height },
				themeId: theme.id,
				colorMode: theme.mode,
				density,
				shots: 'matrix' as const
			}
		}))
	)
);

const sample: Project<Appearance>[] = sampleThemes(rotationSeed()).map((theme) => ({
	name: `sample-${theme.id}`,
	testMatch: smoke,
	snapshotPathTemplate: themeSnapshots,
	use: { ...desktop, themeId: theme.id, colorMode: theme.mode, shots: 'themes' }
}));

export default defineConfig<Appearance>({
	testDir: 'e2e',
	// Pass --update-snapshots to regenerate Linux baselines; never commit macOS/Windows ones.
	forbidOnly: !!process.env.CI,
	retries: 0,
	workers: process.env.CI ? 4 : undefined,
	reporter: [['list'], ['html', { open: 'never' }]],
	webServer: {
		command: 'npm run build && npm run preview -- --port 4173',
		env: { VITE_DEV_ROUTES: '1' },
		port: 4173,
		// A cold `npm run assets` takes ~73 s on CI.
		timeout: 180_000,
		reuseExistingServer: !process.env.CI
	},
	use: { baseURL: 'http://localhost:4173' },
	projects: [
		{
			name: 'core',
			testMatch: /(routes|tokens|harness|theme-gate|coverage)\.test\.ts$/,
			// Keep the baselines committed before the harness existed (`tokens-dark-linux.png`).
			snapshotPathTemplate:
				'{snapshotDir}/{testFileDir}/{testFileName}-snapshots/{arg}{-snapshotSuffix}{ext}'
		},
		...matrix,
		...sample,
		{
			name: 'reduced-motion',
			testMatch: [smoke, /reduced-motion\.test\.ts$/],
			// Motion tokens + axe; its pixels equal riverstone-light's, so no baselines.
			use: { ...desktop, contextOptions: { reducedMotion: 'reduce' }, motion: 'reduce' }
		},
		{ name: 'keyboard-only', testMatch: /\.keyboard\.test\.ts$/, use: desktop },
		// 48 themes × every route: only when asked for (`--project=all-themes` or E2E_ALL_THEMES=1).
		...(allThemes
			? [
					{
						name: 'all-themes',
						testMatch: /all-themes\.test\.ts$/,
						snapshotPathTemplate: themeSnapshots,
						use: desktop
					}
				]
			: [])
	]
});
