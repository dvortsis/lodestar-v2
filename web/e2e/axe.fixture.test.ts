// Axe on real data (X-ui-guards; review B3): /insights over the fixture data set and every step of
// the first-run wizard, against a real `lodestar serve --fixture` server, with no serious or
// critical WCAG 2.2 AA violation. The mock routes can't show what the fixture's numbers, gaps,
// annotations and cohorts render. It reads nothing it writes: the wizard is walked with Skip and
// Continue, which send nothing (its progress lives in sessionStorage), so it runs in `fixture`,
// not `fixture-writes`. It skips unless a fixture server is named (see insights.fixture.test.ts):
//
//   E2E_FIXTURE=1 E2E_FIXTURE_URL=http://127.0.0.1:8291 E2E_FIXTURE_PASSWORD=<pw> \
//     npx playwright test axe.fixture --project=fixture
import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page } from '@playwright/test';
import { WCAG_22_AA } from './a11y';
import { fixtureCookies } from './support/fixture-session';

const url = process.env.E2E_FIXTURE_URL;
const password = process.env.E2E_FIXTURE_PASSWORD;
test.skip(!url || !password, 'set E2E_FIXTURE_URL and E2E_FIXTURE_PASSWORD to run');

type AxePage = ConstructorParameters<typeof AxeBuilder>[0]['page'];
/** Waits for conditions, never for a fixed time; 10 s at least, for the slowest runner. */
const WAIT = { timeout: 15_000 };

/** Fails on serious or critical violations; lesser ones are attached to the report. */
async function expectNoSeriousViolations(page: Page, where: string) {
	const { violations } = await new AxeBuilder({ page: page as unknown as AxePage })
		.withTags(WCAG_22_AA)
		.analyze();
	const describe = (v: (typeof violations)[number]) => [
		`${v.id} (${v.impact ?? 'n/a'}): ${v.help}`,
		`  ${v.helpUrl}`,
		...v.nodes.map((n) => `  target: ${n.target.join(' ')}`)
	];
	const minor = violations.filter((v) => v.impact !== 'serious' && v.impact !== 'critical');
	if (minor.length)
		test.info().annotations.push({
			type: 'axe (moderate or minor)',
			description: `${where}: ${minor.flatMap(describe).join('\n')}`
		});
	const serious = violations.filter((v) => v.impact === 'serious' || v.impact === 'critical');
	expect(serious.flatMap(describe), `serious or critical axe violations on ${where}`).toEqual([]);
}

test.beforeEach(async ({ page }) => {
	// The project's global setup signed in once; this puts its session on the page.
	await page.context().addCookies(fixtureCookies());
});

test('/insights over the fixture data has no serious or critical violations', async ({ page }) => {
	await page.goto(`${url}/insights?range=7d`);
	// The overview has rendered its numbers, its trend with the fixture's gaps and its funnel.
	await expect(page.getByTestId('insights-kpi-new-value')).toBeVisible(WAIT);
	await expect(page.getByTestId('insights-funnel-stored-value')).toBeVisible(WAIT);
	await expect(page.locator('[data-testid^="insights-annotation-"]').first()).toBeVisible(WAIT);
	await expectNoSeriousViolations(page, '/insights (7d)');

	// The trend's table view, which the chart's toggle opens.
	await page.getByTestId('insights-chart-new-toggle').click();
	await expect(page.getByTestId('insights-chart-new-table')).toBeVisible(WAIT);
	await expectNoSeriousViolations(page, '/insights (7d, trend table)');
});

// Every step in order, with its title (en): optional steps are left with Skip, which sends nothing;
// "How it works" has no Skip and saves nothing, so Continue leaves it.
const STEPS: { title: string; leave: 'skip' | 'continue' | null }[] = [
	{ title: 'Network check', leave: 'skip' },
	{ title: 'How it works', leave: 'continue' },
	{ title: 'Crawl intensity', leave: 'skip' },
	{ title: 'Storage & retention', leave: 'skip' },
	{ title: 'Download client', leave: 'skip' },
	{ title: 'Rule packs', leave: 'skip' },
	{ title: 'Blocklists', leave: 'skip' },
	{ title: 'Updates', leave: 'skip' },
	{ title: 'Done', leave: null }
];

test('every first-run wizard step has no serious or critical violations', async ({ page }) => {
	await page.goto(`${url}/welcome`);
	const wizard = page.getByTestId('wizard');
	await expect(wizard).toBeVisible(WAIT);
	for (const { title, leave } of STEPS) {
		await expect(
			wizard.locator('[aria-current="step"]'),
			`the progress list is at ${title}`
		).toContainText(title, WAIT);
		// The step's chunk is in once its primary action is enabled (the last step has none).
		const primary = wizard.getByRole('button', { name: /^(Continue|Save and continue)$/ });
		if (leave) await expect(primary).toBeEnabled(WAIT);
		await expectNoSeriousViolations(page, `/welcome (${title})`);
		if (leave === 'skip') await wizard.getByRole('button', { name: 'Skip this step' }).click();
		else if (leave === 'continue') await primary.click();
	}
});
