// §10.17 reconciliation against the real server over its fixture data set. It imports
// `@playwright/test` directly (no OpenAPI mock) and skips unless a fixture server is named:
//
//   npm run build && cargo build -p lodestar
//   LODESTAR_FIXTURE_PASSWORD=<pw> LODESTAR_HTTP_BIND=127.0.0.1:18091 \
//     target/debug/lodestar serve --fixture <dir>
//   E2E_FIXTURE_URL=http://127.0.0.1:18091 E2E_FIXTURE_PASSWORD=<pw> E2E_FIXTURE_DIR=<dir> \
//     npx playwright test insights.fixture --project=core
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import type { components } from '../src/lib/api/schema';

type Overview = components['schemas']['OverviewResponse'];

const url = process.env.E2E_FIXTURE_URL;
const password = process.env.E2E_FIXTURE_PASSWORD;
const dir = process.env.E2E_FIXTURE_DIR;
test.skip(!url || !password, 'set E2E_FIXTURE_URL and E2E_FIXTURE_PASSWORD to run');

const manifest = () =>
	dir
		? (JSON.parse(readFileSync(join(dir, 'fixture.json'), 'utf8')) as {
				rollups: { new_searchable_24h: number; new_searchable_7d: number };
			})
		: undefined;

async function signIn(page: Page) {
	const res = await page.request.post(`${url}/api/v1/login`, { data: { password } });
	expect(res.ok()).toBe(true);
}
async function overview(page: Page, query: string): Promise<Overview> {
	const res = await page.request.get(`${url}/api/v1/insights/overview?${query}`);
	expect(res.ok()).toBe(true);
	return res.json();
}
const count = (n: number) => new Intl.NumberFormat('en').format(n);
const compact = (n: number) =>
	new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 1 }).format(n);

for (const [range, key] of [
	['24h', 'new_searchable_24h'],
	['7d', 'new_searchable_7d']
] as const) {
	test(`new searchable in ${range} equals the fixture's rollup`, async ({ page }) => {
		const m = manifest();
		test.skip(!m, 'set E2E_FIXTURE_DIR to read fixture.json');
		await signIn(page);
		await page.goto(`${url}/insights?range=${range}`);
		const expected = m!.rollups[key];
		await expect(page.getByTestId('insights-kpi-new-value')).toHaveText(compact(expected));
		expect((await overview(page, `range=${range}`)).kpi.new_searchable).toBe(expected);
	});
}

test('gaps at the paused and stopped spans, with their annotations', async ({ page }) => {
	await signIn(page);
	const data = await overview(page, 'range=7d');
	expect(data.trend.some((p) => p.searchable === null && p.harvested === null)).toBe(true);
	await page.goto(`${url}/insights`);
	for (const kind of new Set(data.annotations.map((a) => a.kind))) {
		await expect(page.getByTestId(`insights-annotation-${kind}`).first()).toBeVisible();
	}
	await page.getByTestId('insights-chart-new-toggle').click();
	const dashes = page
		.getByTestId('insights-chart-new-table')
		.locator('tbody tr td:nth-child(2)', { hasText: /^—$/ });
	expect(await dashes.count()).toBe(data.trend.filter((p) => p.searchable === null).length);
});

test('incomplete cohorts are labelled and a funnel stage equals the API', async ({ page }) => {
	await signIn(page);
	const data = await overview(page, 'range=7d');
	await page.goto(`${url}/insights`);
	const open = data.funnel.filter((c) => !c.final).length;
	await expect(page.locator('[data-testid^="insights-cohort-"][data-testid$="-tag"]')).toHaveCount(
		Math.min(open, 24)
	);
	await expect(page.getByTestId('insights-funnel-stored-value')).toHaveText(
		count(data.funnel_total.stages.stored)
	);
	// The drill-through lands on a URL carrying the same range.
	await page.getByTestId('insights-funnel-stored').locator('a').click();
	await expect(page).toHaveURL(/\/insights\/understand\?range=7d$/);
});
