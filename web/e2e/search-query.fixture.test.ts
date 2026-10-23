// The query bar against the real server (`lodestar serve --fixture`, bench/README.md "Fixture
// backend"), not the OpenAPI mock. Local only until S-e2e wires a fixture job: it skips unless
// E2E_FIXTURE_URL is set, and imports @playwright/test directly so no mock is installed. Run it with
// a copied local config (`testMatch: /\.fixture\.test\.ts$/`, a private port) after
//   npm run build && LODESTAR_HTTP_BIND=127.0.0.1:<port> LODESTAR_FIXTURE_PASSWORD=… \
//     cargo run -p lodestar -- serve --fixture <tmp>
import { expect, test } from '@playwright/test';

const BASE = process.env.E2E_FIXTURE_URL;
const PASSWORD = process.env.LODESTAR_FIXTURE_PASSWORD ?? '';

test.skip(!BASE, 'E2E_FIXTURE_URL is not set (fixture backend specs are local only)');
test.use({ baseURL: BASE });

test.beforeEach(async ({ page }) => {
	const res = await page.request.post('/api/v1/login', { data: { password: PASSWORD } });
	expect(res.ok(), `login: ${res.status()}`).toBe(true);
});

const bar = (page: import('@playwright/test').Page) =>
	page.getByRole('combobox', { name: 'Search torrents' });

test('lantern type:movie shows a Type chip; removing it leaves ?q=lantern', async ({ page }) => {
	await page.goto('/search');
	await bar(page).click();
	await page.keyboard.type('lantern type:movie');
	await page.keyboard.press('Enter');
	const chip = page.getByRole('button', { name: 'Remove filter: Type: Movie' });
	await expect(chip).toBeVisible();
	await expect(bar(page).locator('.qt-field')).toHaveText('type:');
	await chip.click();
	await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('lantern');
	await expect(bar(page)).toHaveText('lantern');
	await expect(chip).toHaveCount(0);
});

test('seeders:>=1 shows the server’s "not available yet" message', async ({ page }) => {
	await page.goto('/search');
	await bar(page).click();
	await page.keyboard.type('seeders:>=1');
	await page.keyboard.press('Enter');
	await expect(bar(page)).toHaveAttribute('aria-invalid', 'true');
	await expect(page.locator('#search-query-message')).toContainText(
		/Can't read this query:.*swarm health/
	);
});
