// The query bar against the real server (`lodestar serve --fixture`, bench/README.md "Fixture
// backend"), not the OpenAPI mock. Local only until S-e2e wires a fixture job: it skips unless
// E2E_FIXTURE_URL is set, and imports @playwright/test directly so no mock is installed. Run it with
// a copied local config (`testMatch: /\.fixture\.test\.ts$/`, a private port) after
//   npm run build && LODESTAR_HTTP_BIND=127.0.0.1:<port> LODESTAR_FIXTURE_PASSWORD=… \
//     cargo run -p lodestar -- serve --fixture <tmp>
import { expect, test, type Page } from '@playwright/test';

const BASE = process.env.E2E_FIXTURE_URL;
const PASSWORD = process.env.LODESTAR_FIXTURE_PASSWORD ?? '';

test.skip(!BASE, 'E2E_FIXTURE_URL is not set (fixture backend specs are local only)');

// One sign-in for the file: the server rate-limits logins, and the tests share the session.
test.describe.configure({ mode: 'serial' });
let page: Page;
test.beforeAll(async ({ browser }) => {
	const context = await browser.newContext({ baseURL: BASE });
	const res = await context.request.post('/api/v1/login', { data: { password: PASSWORD } });
	expect(res.ok(), `login: ${res.status()}`).toBe(true);
	page = await context.newPage();
});
test.afterAll(() => page?.context().close());

const bar = (page: Page) => page.getByRole('combobox', { name: 'Search torrents' });

test('lantern type:movie is highlighted, gets a Type chip, and removing it leaves ?q=lantern', async () => {
	await page.goto('/search');
	await bar(page).click();
	await page.keyboard.type('lantern type:movie');
	await expect(bar(page).locator('.qt-field')).toHaveText('type:');
	await expect(bar(page).locator('.qt-value')).toHaveText('movie');
	await page.keyboard.press('Enter');
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('lantern type:movie');
	await expect(bar(page)).not.toHaveAttribute('aria-invalid');
	await expect(page.getByRole('alert')).toHaveCount(0);
	const chip = page.getByRole('button', { name: 'Remove filter: Type: Movie' });
	await expect(chip).toBeVisible();
	await chip.click();
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('lantern');
	await expect(bar(page)).toHaveText('lantern');
	await expect(chip).toHaveCount(0);
});

test('seeders:>=1 parses now that swarm health has landed (#48)', async () => {
	await page.goto('/search');
	await bar(page).click();
	await page.keyboard.type('seeders:>=1');
	await expect(bar(page).locator('.qt-range')).toHaveText('>=1');
	await page.keyboard.press('Enter');
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('seeders:>=1');
	await expect(bar(page)).not.toHaveAttribute('aria-invalid');
	await expect(
		page.getByRole('button', { name: 'Remove filter: Seeders: at least 1' })
	).toBeVisible();
});

test('sig: shows the server’s "not available yet" message, marked in the bar', async () => {
	await page.goto('/search');
	await bar(page).click();
	await page.keyboard.type('sig:7a9f3c12');
	await page.keyboard.press('Enter');
	await expect(bar(page)).toHaveAttribute('aria-invalid', 'true');
	await expect(page.locator('#search-query-message')).toContainText(
		/Can't read this query:\s+sig: arrives with grouping keys/
	);
	await expect(bar(page).locator('.qt-error')).toHaveText('7a9f3c12');
});

test('a mistyped field is marked and the server suggests the right one', async () => {
	await page.goto('/search');
	await bar(page).click();
	await page.keyboard.type('nme:x');
	await page.keyboard.press('Enter');
	await expect(page.locator('#search-query-message')).toContainText('did you mean name:?');
	await expect(bar(page).locator('.qt-error')).toHaveText('nme:');
});
