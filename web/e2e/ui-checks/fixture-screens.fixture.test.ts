// UI check (review 02 §4b, 03 "a fixture-backed axe and screenshot run for five key screens"):
// five screens against a real `lodestar serve --fixture` server, not the mock, each one loaded
// (no aria-busy, no error banner), free of serious or critical WCAG 2.2 AA violations, and
// matching a Riverstone-light baseline at 1366 px. The mock routes cannot show what the fixture's
// numbers, names and gaps look like on screen.
//   search /search?q=lantern, discover, alerts, one torrent's page, settings/general.
// axe.fixture.test.ts scans /insights and the wizard, not these, so each is scanned here. Insights
// was tried and dropped: its page differs between two runs on the same server (its height moved by
// 100 px or more), so it cannot hold a baseline.
// The clock is frozen at the fixture's own "now" (its manifest `anchor`), so relative times are
// the same on every run. The masked parts change with the fixture build: the crawler status pill
// (live figures), the torrent page's absolute times (discovered, last seen, last scrape) and
// Search's "Discovered" histogram (bars and axis dates), which the fixture builds from the hour
// it was made in.
// The `fixture` project runs after `fixture-writes`, so these screens are captured with the rules
// and crawler settings those specs leave behind. A change to filters-flows or settings-form can
// change these baselines. Baselines are
// Linux-only and come from CI's marker run, like every other baseline (web/e2e/README.md,
// "Updating snapshots", the fixture baselines paragraph). Skips unless a fixture server is named:
//
//   E2E_FIXTURE=1 E2E_FIXTURE_URL=http://127.0.0.1:8291 E2E_FIXTURE_DIR=/tmp/lodestar-fixture \
//   E2E_FIXTURE_PASSWORD=<pw> npx playwright test fixture-screens --project=fixture
import { readFileSync } from 'node:fs';
import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Locator, type Page } from '@playwright/test';
import { WCAG_22_AA } from '../a11y';
import { fixtureCookies } from '../support/fixture-session';

const url = process.env.E2E_FIXTURE_URL;
const password = process.env.E2E_FIXTURE_PASSWORD;
const dir = process.env.E2E_FIXTURE_DIR ?? '';
test.skip(
	!url || !password || !dir,
	'set E2E_FIXTURE_URL, E2E_FIXTURE_PASSWORD and E2E_FIXTURE_DIR to run'
);
test.use({ baseURL: url, viewport: { width: 1366, height: 900 } });

type AxePage = ConstructorParameters<typeof AxeBuilder>[0]['page'];
/** Waits for conditions, never for a fixed time; 10 s at least, for the slowest runner. */
const WAIT = { timeout: 15_000 };

interface Manifest {
	anchor: number;
	torrents: { hash: string; name: string; swarm: null | { seeders: number } }[];
}
const manifest = (): Manifest => JSON.parse(readFileSync(`${dir}/fixture.json`, 'utf8'));

test.beforeEach(async ({ page }) => {
	await page.context().addCookies(fixtureCookies());
	// The fixture's own "now": relative times ("2 days ago") are computed from the clock.
	await page.clock.setFixedTime(manifest().anchor * 1000);
});

/** The loaded screen, not a placeholder: no aria-busy, no error banner, its own controls shown. */
async function loaded(page: Page, ready: Locator, where: string) {
	const main = page.locator('main');
	await expect(main.locator('[aria-busy="true"]'), `${where}: still loading`).toHaveCount(0, WAIT);
	await expect(ready.first(), `${where}: its content`).toBeVisible(WAIT);
	await expect(
		main.locator('.sr-only', { hasText: /^Error:/ }),
		`${where}: an error banner`
	).toHaveCount(0);
}

async function noSeriousViolations(page: Page, where: string) {
	const { violations } = await new AxeBuilder({ page: page as unknown as AxePage })
		.withTags(WCAG_22_AA)
		.analyze();
	const serious = violations.filter((v) => v.impact === 'serious' || v.impact === 'critical');
	expect(
		serious.map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(' ')).join(', ')})`),
		`serious or critical axe violations on ${where}`
	).toEqual([]);
}

/**
 * The same options as `expectPageSnapshot` (e2e/snapshot.ts, tolerance included), plus a mask for
 * what changes between runs. That helper takes no mask and lives outside this slice, so the
 * options are repeated here; keep the two in step.
 */
async function snapshot(page: Page, name: string) {
	await page.evaluate(() => document.fonts.ready);
	await expect(page).toHaveScreenshot(`${name}.png`, {
		fullPage: true,
		animations: 'disabled',
		caret: 'hide',
		maxDiffPixelRatio: 0.001,
		mask: [
			page.getByTestId('status-pill'),
			page.getByTestId('detail-discovered'),
			page.getByTestId('detail-last-seen'),
			page.getByTestId('detail-scrape-at'),
			// Search's "Discovered" histogram: its bars and its two axis dates come from the fixture's build.
			page.getByTestId('facet-discovered-histogram')
		]
	});
}

const SCREENS: {
	name: string;
	path: () => string;
	ready: (page: Page) => Locator;
}[] = [
	{
		name: 'search',
		path: () => '/search?q=lantern',
		ready: (page) => page.getByRole('row').filter({ hasText: /Lantern/i })
	},
	{
		name: 'discover',
		path: () => '/discover',
		ready: (page) => page.locator('[data-testid^="discover-row-"]')
	},
	{
		name: 'alerts',
		path: () => '/alerts',
		ready: (page) => page.locator('[data-testid^="alerts-name-"]')
	},
	{
		name: 'torrent',
		path: () => `/t/${(manifest().torrents.find((t) => t.swarm) ?? manifest().torrents[0]!).hash}`,
		ready: (page) => page.getByRole('heading', { level: 1 })
	},
	{
		name: 'settings',
		path: () => '/settings/general',
		ready: (page) => page.locator('[data-field]')
	}
];

for (const screen of SCREENS) {
	test(`${screen.name}: loaded, axe and matching its baseline`, async ({ page }) => {
		await page.goto(screen.path());
		await loaded(page, screen.ready(page), screen.name);
		await noSeriousViolations(page, screen.name);
		await snapshot(page, screen.name);
	});
}
