import { defineConfig, type Project } from '@playwright/test';
import type { Appearance } from './e2e/support/test';
import { BREAKPOINTS, DENSITIES, DESKTOP } from './e2e/support/matrix';
import { RIVERSTONE, rotationSeed, sampleThemes } from './e2e/support/themes';

// Projects (filter with `--project=<name>`; see e2e/README.md):
//   core                          pre-existing specs + harness self-tests + theme gate
//   bp<width>-<theme>-<density>   smoke (axe + snapshot) per breakpoint × Riverstone light/dark × density
//   sample-<theme-id>             the same smoke at 1366 for the rotating 6 non-Riverstone themes
//   reduced-motion                smoke + motion tokens with prefers-reduced-motion: reduce
//   keyboard-only                 *.keyboard.test.ts flows (no pointer allowed)
//   all-themes                    axe on every route for all 48 themes (nightly; opt-in, see below)

const allThemes = !!process.env.E2E_ALL_THEMES || process.argv.join(' ').includes('all-themes');
const smoke = /smoke\.test\.ts$/;
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
				density
			}
		}))
	)
);

const sample: Project<Appearance>[] = sampleThemes(rotationSeed()).map((theme) => ({
	name: `sample-${theme.id}`,
	testMatch: smoke,
	use: { ...desktop, themeId: theme.id, colorMode: theme.mode }
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
			use: { ...desktop, contextOptions: { reducedMotion: 'reduce' }, motion: 'reduce' }
		},
		{ name: 'keyboard-only', testMatch: /\.keyboard\.test\.ts$/, use: desktop },
		// 48 themes × every route: only when asked for (`--project=all-themes` or E2E_ALL_THEMES=1).
		...(allThemes ? [{ name: 'all-themes', testMatch: /all-themes\.test\.ts$/, use: desktop }] : [])
	]
});
