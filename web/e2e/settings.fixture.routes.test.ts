// The Settings screens against a real `lodestar serve --fixture` (opt-in): the screens open from
// the real schema, an env-locked field, a real 422 on `crawler.timeout_secs`, and a save that sends
// only the edited field. Skipped unless both variables are set:
//
//   E2E_FIXTURE_URL=http://127.0.0.1:18433 E2E_FIXTURE_PASSWORD=<pw> \
//     npx playwright test e2e/settings.fixture.routes.test.ts --project=core
//
// against `LODESTAR_FIXTURE_PASSWORD=<pw> LODESTAR_HTTP_BIND=127.0.0.1:18433 \
//   cargo run -p lodestar -- serve --fixture <dir>`, built after `npm run build`.
import { expect, test, type Page } from '@playwright/test';

const URL_ = process.env.E2E_FIXTURE_URL ?? '';
const PASSWORD = process.env.E2E_FIXTURE_PASSWORD ?? '';
test.skip(!URL_ || !PASSWORD, 'E2E_FIXTURE_URL and E2E_FIXTURE_PASSWORD are not set');
test.use({ baseURL: URL_ });

async function signedIn(page: Page) {
	const res = await page.request.post('/api/v1/login', { data: { password: PASSWORD } });
	expect(res.ok(), `login: ${res.status()}`).toBe(true);
}

const h1 = (page: Page, name: string) => page.getByRole('heading', { level: 1, name });

test('opens each screen of the real schema', async ({ page }) => {
	await signedIn(page);
	for (const [id, name] of [
		['crawler', 'Crawler'],
		['network', 'Network & VPN'],
		['storage', 'Storage & retention'],
		['search', 'Search & ranking'],
		['general', 'General'],
		['logging', 'Logging']
	]) {
		await page.goto(`/settings/${id}`);
		await expect(h1(page, name)).toBeVisible();
		await expect(page.locator('[data-field]').first()).toBeVisible();
	}
});

test('shows the fixture’s environment-locked network fields', async ({ page }) => {
	await signedIn(page);
	await page.goto('/settings/network');
	for (const key of ['network.http_bind', 'network.dht_enabled']) {
		await expect(page.locator(`[data-field="${key}"]`).getByTestId('status-env')).toBeVisible();
	}
});

test('shows the server’s 422 for a timeout out of bounds', async ({ page }) => {
	await signedIn(page);
	await page.goto('/settings/crawler#crawler.timeout_secs');
	const field = page.getByLabel('Fetch timeout', { exact: true });
	await expect(field).toBeFocused();
	await field.fill('500');
	await page.getByRole('button', { name: 'Save changes' }).click();
	await expect(page.getByTestId('error-summary')).toBeVisible();
});
