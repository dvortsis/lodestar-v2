import { readdirSync, readFileSync } from 'node:fs';
import { join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig, type PlaywrightTestOptions, type Project } from '@playwright/test';
import type { Appearance } from './e2e/support/test';
import { BREAKPOINTS, DENSITIES, DESKTOP } from './e2e/support/matrix';
import { FIXTURE_SPEC, SPEC_FILE, specProblems, type SpecFile } from './scripts/e2e-spec-guard';
import { RIVERSTONE, rotationSeed, sampleThemes } from './e2e/support/themes';

// Projects (filter with `--project=<name>`; see e2e/README.md):
//   core                          pre-existing specs + harness self-tests + theme gate
//   bp<width>-<theme>-<density>   smoke + illustrated states per breakpoint × Riverstone light/dark × density: axe on every
//                                 route, baselines for key routes (Compact: density-aware routes)
//   sample-<theme-id>             smoke at 1366 for the rotating 6 non-Riverstone themes; key-route
//                                 baselines come from the shared 46-theme set
//   reduced-motion                axe + motion tokens with prefers-reduced-motion: reduce (no baselines)
//   keyboard-only                 *.keyboard.test.ts flows (no pointer allowed)
//   fixture-writes               the *.fixture.test.ts specs that change server state (`filters-flows`,
//                                 `settings-form`); `fixture` depends on it, so it runs first
//   fixture                       the other *.fixture.test.ts against a real `lodestar serve --fixture`
//                                 server (opt-in: --project=fixture or E2E_FIXTURE=1; nightly in e2e.yml)
//   docs                          documentation screenshots into docs/user/images (opt-in:
//                                 --project=docs or E2E_DOCS=1; `npm run docs:shots`)
//   all-themes                    axe on every route in all 48 themes + the 46-theme key-route
//                                 baselines (nightly; opt-in: --project=all-themes or E2E_ALL_THEMES=1)

// Local override, never set in CI: run an already-installed Chromium instead of the build this
// @playwright/test pins (cloud sessions ship a different one). Unset = unchanged behaviour.
const chromiumPath = process.env.PW_CHROMIUM_PATH;

// Port for the preview server; set PW_PORT so parallel worktrees don't share one.
const port = Number(process.env.PW_PORT || 4173);
if (!Number.isInteger(port) || port < 1 || port > 65535)
	throw new Error('PW_PORT must be a port number');

const allThemes = !!process.env.E2E_ALL_THEMES || process.argv.join(' ').includes('all-themes');
const fixture =
	!!process.env.E2E_FIXTURE ||
	process.argv
		.join(' ')
		.split(/[ =]/)
		.some((a) => a === 'fixture' || a === 'fixture-writes');
const docs = !!process.env.E2E_DOCS || process.argv.join(' ').split(/[ =]/).includes('docs');
const smoke = /smoke\.test\.ts$/;
const matrixSpecs = [smoke, /states\.test\.ts$/];
// The theme set's baselines (key screens × 46 non-Riverstone themes, at 1366) are shared by the
// rotating `sample-*` projects and the nightly `all-themes` project: no project name in the path.
const themeSnapshots = '{snapshotDir}/{testFileDir}/theme-snapshots/{arg}{-snapshotSuffix}{ext}';
// Names of the real-server specs: `fixture` runs these, so a file that reads E2E_FIXTURE_URL must
// carry the suffix (a name ending `.fixture.routes.test.ts` matches `core`, skips silently, never runs).
const fixtureSpec = FIXTURE_SPEC;
// Real-server specs that change server state (rules, settings). They run first, then the
// read-only ones (`fixture` depends on this), so a count read never races a write in another worker.
const fixtureWriteSpecs = /(filters-flows|settings-form)\.fixture\.test\.ts$/;
const coreSpecs = /(routes|tokens|harness|theme-gate|coverage)\.test\.ts$/;
const keyboardSpecs = /\.keyboard\.test\.ts$/;
const reducedMotionSpecs = /reduced-motion\.test\.ts$/;
const allThemesSpecs = /all-themes\.test\.ts$/;
const desktop = { viewport: { width: DESKTOP.width, height: DESKTOP.height } };

// The options a project's `use` may carry: Playwright's own plus this suite's `Appearance`.
type Proj = Project<PlaywrightTestOptions & Appearance>;

const matrix: Project<Appearance>[] = BREAKPOINTS.flatMap((bp) =>
	RIVERSTONE.flatMap((theme) =>
		DENSITIES.map((density) => ({
			name: `bp${bp.name}-${theme.id}-${density}`,
			testMatch: matrixSpecs,
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

const core: Proj = {
	name: 'core',
	testMatch: coreSpecs,
	// Keep the baselines committed before the harness existed (`tokens-dark-linux.png`).
	snapshotPathTemplate:
		'{snapshotDir}/{testFileDir}/{testFileName}-snapshots/{arg}{-snapshotSuffix}{ext}'
};
const reducedMotion: Proj = {
	name: 'reduced-motion',
	testMatch: [smoke, reducedMotionSpecs],
	// Motion tokens + axe; its pixels equal riverstone-light's, so no baselines.
	use: { ...desktop, contextOptions: { reducedMotion: 'reduce' }, motion: 'reduce' }
};
const keyboardOnly: Proj = {
	name: 'keyboard-only',
	testMatch: keyboardSpecs,
	use: desktop
};
// Specs that need the real server: only when asked for (see e2e/README.md). The ones that change
// server state run first; the read-only ones wait for them (`dependencies`), so CI's
// `--project=fixture` runs both.
const fixtureWrites: Proj = {
	name: 'fixture-writes',
	testMatch: fixtureWriteSpecs,
	use: desktop
};
const fixtureReads: Proj = {
	name: 'fixture',
	testMatch: fixtureSpec,
	testIgnore: fixtureWriteSpecs,
	dependencies: ['fixture-writes'],
	use: desktop
};
// Documentation screenshots: only when asked for (`npm run docs:shots`); CI never runs them.
const docsShots: Proj = {
	name: 'docs',
	testMatch: /docs\/.*\.docs\.ts$/,
	use: { viewport: { width: 1366, height: 900 }, deviceScaleFactor: 1 }
};
// 48 themes × every route: only when asked for (`--project=all-themes` or E2E_ALL_THEMES=1).
const allThemesProject: Proj = {
	name: 'all-themes',
	testMatch: allThemesSpecs,
	snapshotPathTemplate: themeSnapshots,
	use: desktop
};

// Every project, the opt-in ones included: the guard below reads this same list, so `projects`
// (what runs) and what the guard counts as covered cannot drift apart.
const optIn = (on: boolean, ...p: Proj[]) => (on ? p : []);
const everyProject: Proj[] = [
	core,
	...matrix,
	...sample,
	reducedMotion,
	keyboardOnly,
	fixtureWrites,
	fixtureReads,
	docsShots,
	allThemesProject
];
const projects = [
	core,
	...matrix,
	...sample,
	reducedMotion,
	keyboardOnly,
	...optIn(fixture, fixtureWrites, fixtureReads),
	...optIn(docs, docsShots),
	...optIn(allThemes, allThemesProject)
];

// Guard: every spec under e2e/ (everything Playwright's default `testMatch` would take, not just
// `*.test.ts`) must be run by some project, the opt-in ones included: `testMatch` matches it and
// `testIgnore` does not. A spec that reads E2E_FIXTURE_URL must run only in the fixture projects
// (scripts/e2e-spec-guard.ts). A spec that fits no project never runs, and nothing says so.
function checkSpecsHaveProjects() {
	const root = fileURLToPath(new URL('./e2e', import.meta.url));
	const files: SpecFile[] = [];
	const walk = (dir: string) => {
		for (const entry of readdirSync(dir, { withFileTypes: true })) {
			const path = join(dir, entry.name);
			if (entry.isDirectory()) {
				if (entry.name !== 'node_modules') walk(path);
			} else if (SPEC_FILE.test(entry.name))
				files.push({
					path: relative(root, path).split(sep).join('/'),
					source: readFileSync(path, 'utf8')
				});
		}
	};
	walk(root);
	const problems = specProblems(files, everyProject);
	if (problems.length) throw new Error('e2e spec/project mismatch:\n' + problems.join('\n'));
}
checkSpecsHaveProjects();

export default defineConfig<Appearance>({
	testDir: 'e2e',
	// The fixture project signs in once here; see e2e/support/fixture-session.ts.
	...(fixture ? { globalSetup: './e2e/support/fixture-session.ts' } : {}),
	// Pass --update-snapshots to regenerate Linux baselines; never commit macOS/Windows ones.
	forbidOnly: !!process.env.CI,
	retries: 0,
	workers: process.env.CI ? 4 : undefined,
	reporter: [['list'], ['html', { open: 'never' }]],
	webServer: {
		command: 'npm run build && npm run preview -- --port ' + port + ' --strictPort',
		env: { VITE_DEV_ROUTES: '1' },
		port,
		// A cold `npm run assets` takes ~73 s on CI.
		timeout: 180_000,
		reuseExistingServer: !process.env.CI
	},
	use: {
		baseURL: `http://localhost:${port}`,
		...(chromiumPath ? { launchOptions: { executablePath: chromiumPath } } : {})
	},
	projects
});
