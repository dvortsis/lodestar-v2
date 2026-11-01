// Against the real server (`lodestar serve --fixture`, bench/README.md), local only: skipped unless
// E2E_FIXTURE_URL names it (and E2E_FIXTURE_PASSWORD its sign-in). Plain Playwright `test`, not
// the harness one: no OpenAPI mock. /dev/shortcuts exists only in a build made with
// VITE_DEV_ROUTES=1, which the server must embed.
import { expect, test, type Page } from '@playwright/test';

const url = process.env.E2E_FIXTURE_URL;
const password = process.env.E2E_FIXTURE_PASSWORD ?? 'correct horse battery';

test.skip(!url, 'set E2E_FIXTURE_URL to run against serve --fixture');

// One sign-in for the file: the server rate-limits logins (a burst of 5 per address), and the
// fixture files together sign in 5 times, so a second sign-in here would be refused.
test.describe.configure({ mode: 'serial' });
let page: Page;
test.beforeAll(async ({ browser }) => {
	const context = await browser.newContext({ baseURL: url });
	const res = await context.request.post('/api/v1/login', { data: { password } });
	expect(res.ok(), `login: ${res.status()}`).toBe(true);
	page = await context.newPage();
});
test.afterAll(() => page?.context().close());

test('a saved remap holds after a reload, and the server refuses a clash', async () => {
	// Start from defaults.
	await page.request.patch('/api/v1/settings/preferences', { data: { shortcut_keys: {} } });
	await page.goto('/dev/shortcuts');
	await page.getByRole('button', { name: 'Change the key for Next result' }).click();
	await page.keyboard.press('n');
	await expect(page.getByText('Saved.')).toBeVisible();

	await page.reload();
	await expect(page.getByTestId('shortcut-key-results.next')).toContainText('N');

	// A clash the editor would have caught, sent straight to the server.
	const res = await page.request.patch('/api/v1/settings/preferences', {
		data: { shortcut_keys: { 'results.next': 'k' } }
	});
	expect(res.status()).toBe(422);
	const error = (await res.json()).error as { message: string };
	expect(error.message).toContain('both use k');
});

test('a clash the editor cannot see is refused with a 422 and the message shows under its row', async () => {
	await page.request.patch('/api/v1/settings/preferences', { data: { shortcut_keys: {} } });
	await page.goto('/dev/shortcuts');
	await expect(page.getByRole('switch', { name: 'Single-key shortcuts' })).toBeVisible();
	// Another client stores p for an action the editor does not list, then this tab (reloaded, so
	// the revision is current) takes p for Next result.
	await page.request.patch('/api/v1/settings/preferences', {
		data: { shortcut_keys: { 'custom.thing': 'p' } }
	});
	await page.reload();
	await page.getByRole('button', { name: 'Change the key for Next result' }).click();
	await page.keyboard.press('p');
	await expect(page.getByTestId('shortcut-row-results.next')).toContainText(/both use p/);
	await page.request.patch('/api/v1/settings/preferences', { data: { shortcut_keys: {} } });
});
