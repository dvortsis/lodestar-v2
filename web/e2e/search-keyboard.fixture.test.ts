// The v0.5 path, keyboard only, against the real server (`lodestar serve --fixture`, bench/README.md
// "Fixture backend"): search → results → open the drawer → copy the magnet. The G-v05 gate reuses
// it. Local only until S-e2e wires a fixture job: it skips unless E2E_FIXTURE_URL is set, and
// imports @playwright/test directly so no mock is installed. Run it as search-query.fixture.test.ts
// says. Nothing here clicks: every step is a key.
import { expect as base, test, type Locator, type Page } from '@playwright/test';

const expect = base.configure({ timeout: 10_000 });
const BASE = process.env.E2E_FIXTURE_URL;
const PASSWORD = process.env.LODESTAR_FIXTURE_PASSWORD ?? '';

test.skip(!BASE, 'E2E_FIXTURE_URL is not set (fixture backend specs are local only)');

let page: Page;
test.beforeAll(async ({ browser }) => {
	const context = await browser.newContext({
		baseURL: BASE,
		viewport: { width: 1366, height: 1000 }
	});
	const res = await context.request.post('/api/v1/login', { data: { password: PASSWORD } });
	expect(res.ok(), `login: ${res.status()}`).toBe(true);
	page = await context.newPage();
	// The page records what it copies: the system clipboard is shared by every worker.
	await page.addInitScript(() => {
		const w = window as unknown as { __copied: string[] };
		w.__copied = [];
		Object.defineProperty(navigator, 'clipboard', {
			value: { writeText: async (text: string) => void w.__copied.push(text) }
		});
	});
});
test.afterAll(() => page?.context().close());

const copied = () =>
	page.evaluate(() => (window as unknown as { __copied: string[] }).__copied.at(-1) ?? null);

/**
 * Tab until `target` has focus, failing after a bounded number of presses. The facet rail comes
 * before the results at this width, a stop per value and its exclude button, hence the bound.
 */
async function tabTo(target: Locator) {
	for (let i = 0; i < 300; i++) {
		if (await target.evaluate((el) => el === document.activeElement)) return;
		await page.keyboard.press('Tab');
	}
	await expect(target).toBeFocused();
}

test('/ lantern, Enter; Tab to the first row, Enter opens the drawer, c copies its magnet', async () => {
	await page.goto('/search');
	await expect(page.locator('#boot')).toHaveCount(0);
	await page.keyboard.press('/');
	const bar = page.getByRole('combobox', { name: 'Search torrents' });
	await expect(bar).toBeFocused();
	await page.keyboard.type('lantern');
	await page.keyboard.press('Enter');
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('lantern');

	const grid = page.getByRole('grid', { name: 'Matching torrents' });
	const first = grid.getByRole('row').first();
	await expect(first).toBeVisible();
	await expect(page.getByTestId('results-count')).toHaveText(/^\d[\d,]* results?$/);
	await tabTo(first);
	await page.keyboard.press('Enter');
	await expect(page.getByTestId('drawer')).toBeVisible();
	await expect(page).toHaveURL(/\/t\/[0-9a-f]{40}/);
	const hash = /\/t\/([0-9a-f]{40})/.exec(page.url())![1]!;

	await page.keyboard.press('c');
	await expect(page.getByText('Magnet copied')).toBeVisible();
	await expect.poll(copied).toMatch(new RegExp(`^magnet:\\?xt=urn:btih:${hash}`, 'i'));
});
