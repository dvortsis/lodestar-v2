// The Settings form engine against a real `lodestar serve --fixture` (opt-in): a real two-context
// 409 and merge, the fixture's env-locked network fields, a real 422 bounds message, and Undo
// restoring the previous source. Skipped unless both variables are set:
//
//   E2E_FIXTURE_URL=http://127.0.0.1:18092 E2E_FIXTURE_PASSWORD=<pw> \
//     npx playwright test e2e/settings-form.fixture.routes.test.ts --project=core
//
// against `LODESTAR_FIXTURE_PASSWORD=<pw> LODESTAR_HTTP_BIND=127.0.0.1:18092 \
//   target/debug/lodestar serve --fixture <dir>`, built after `VITE_DEV_ROUTES=1 npm run build`.
import { expect, test, type Browser, type Page } from '@playwright/test';

const URL_ = process.env.E2E_FIXTURE_URL ?? '';
const PASSWORD = process.env.E2E_FIXTURE_PASSWORD ?? '';
test.skip(!URL_ || !PASSWORD, 'E2E_FIXTURE_URL and E2E_FIXTURE_PASSWORD are not set');
test.use({ baseURL: URL_ });

const PAGE = '/dev/settings-form?sections=crawler,network&id=fixture';

async function signedIn(browser: Browser): Promise<Page> {
	const context = await browser.newContext({ baseURL: URL_ });
	const page = await context.newPage();
	const res = await page.request.post('/api/v1/login', { data: { password: PASSWORD } });
	expect(res.ok(), `login: ${res.status()}`).toBe(true);
	return page;
}

/** Removes the values these tests save, so each run starts from the fixture's own. */
async function resetSaved(page: Page) {
	const current = await (await page.request.get('/api/v1/settings/crawler')).json();
	const res = await page.request.patch('/api/v1/settings', {
		data: { crawler: { if_revision: current.revision, intensity: null, timeout_secs: null } }
	});
	expect(res.ok(), `reset: ${res.status()}`).toBe(true);
}

async function open(page: Page) {
	await page.goto(PAGE);
	await expect(page.getByRole('heading', { level: 1, name: 'Settings form' })).toBeVisible();
	await expect(page.locator('[data-field="crawler.intensity"]')).toBeVisible();
}

const intensity = (page: Page, value: string) =>
	page.locator('[data-field="crawler.intensity"]').getByRole('radio', { name: value });

test.describe('settings form on serve --fixture', () => {
	test.describe.configure({ mode: 'serial' });

	test('merges a real 409 from another tab', async ({ browser }) => {
		const a = await signedIn(browser);
		const b = await signedIn(browser);
		await resetSaved(a);
		await open(a);
		await open(b);
		await intensity(a, 'low').check();
		await intensity(b, 'aggressive').check();
		await b.getByTestId('save-bar').getByRole('button', { name: 'Save changes' }).click();
		await expect(b.getByTestId('save-bar')).toContainText('Saved');

		await a.getByTestId('save-bar').getByRole('button', { name: 'Save changes' }).click();
		const conflict = a.locator('[data-field="crawler.intensity"]').getByTestId('conflict');
		await expect(conflict).toContainText('Changed elsewhere to Aggressive');
		await conflict.getByRole('button', { name: 'Keep mine' }).click();
		await a.getByTestId('save-bar').getByRole('button', { name: 'Save changes' }).click();
		await expect(a.getByTestId('save-bar')).toContainText('Saved');
		const saved = await (await a.request.get('/api/v1/settings/crawler')).json();
		expect(saved.fields.intensity).toMatchObject({ value: 'low', source: 'db' });
	});

	test('shows the fixture’s env-locked network fields', async ({ browser }) => {
		const page = await signedIn(browser);
		await open(page);
		for (const [key, variable] of [
			['network.http_bind', 'LODESTAR_HTTP_BIND'],
			['network.dht_enabled', 'LODESTAR_DHT_ENABLED']
		]) {
			const field = page.locator(`[data-field="${key}"]`);
			await expect(field.getByTestId('status-env')).toContainText(`Set by ${variable}`);
		}
	});

	test('maps a real 422 bounds refusal onto the field', async ({ browser }) => {
		const page = await signedIn(browser);
		await resetSaved(page);
		// Widen the schema the page sees, so the client lets 500 through and the server answers.
		await page.route('**/api/v1/settings', async (route) => {
			if (route.request().method() !== 'GET') return route.fallback();
			const res = await route.fetch();
			const index = await res.json();
			for (const s of index.sections) delete s.schema.properties?.timeout_secs?.maximum;
			await route.fulfill({ response: res, json: index });
		});
		await open(page);
		await page
			.getByText(/^Advanced/)
			.first()
			.click();
		const field = page.locator('[data-field="crawler.timeout_secs"]');
		await field.getByRole('textbox').fill('500');
		await page.getByTestId('save-bar').getByRole('button', { name: 'Save changes' }).click();
		const summary = page.getByTestId('error-summary');
		await expect(summary).toBeVisible();
		await expect(field.getByRole('textbox')).toHaveAttribute('aria-invalid', 'true');
		const message = (await summary.getByRole('link').textContent()) ?? '';
		expect(message).toMatch(/120/);
		console.log(`server 422 message: ${message}`);
	});

	test('Undo restores the previous source', async ({ browser }) => {
		const page = await signedIn(browser);
		await resetSaved(page);
		await open(page);
		await page
			.getByText(/^Advanced/)
			.first()
			.click();
		const field = page.locator('[data-field="crawler.timeout_secs"]');
		await expect(field.getByRole('img', { name: 'Changed from default' })).toHaveCount(0);
		await field.getByRole('textbox').fill('30');
		await page.getByTestId('save-bar').getByRole('button', { name: 'Save changes' }).click();
		await expect(field.getByRole('img', { name: 'Changed from default' })).toBeVisible();
		await page.getByTestId('save-bar').getByRole('button', { name: 'Undo' }).click();
		await expect(field.getByRole('img', { name: 'Changed from default' })).toHaveCount(0);
		const now = await (await page.request.get('/api/v1/settings/crawler')).json();
		expect(now.fields.timeout_secs.source).toBe('default');
	});
});
