// Against the real server (`lodestar serve --fixture`, bench/README.md), local only: skipped unless
// E2E_FIXTURE_URL names it (and E2E_FIXTURE_PASSWORD its sign-in). Plain Playwright `test`, not
// the harness one: no OpenAPI mock.
import { expect, test } from '@playwright/test';

const url = process.env.E2E_FIXTURE_URL;
const password = process.env.E2E_FIXTURE_PASSWORD ?? 'correct horse battery';

test.skip(!url, 'set E2E_FIXTURE_URL to run against serve --fixture');
test.use({ baseURL: url });

test.beforeEach(async ({ page }) => {
	await page.goto('/login');
	await page.getByLabel('Password').fill(password);
	await page.getByRole('button', { name: 'Sign in' }).click();
	await page.waitForURL((u) => !u.pathname.startsWith('/login'));
});

test('a saved remap holds after a reload, and the server refuses a clash', async ({
	page,
	request
}) => {
	// Start from defaults.
	await page.request.patch('/api/v1/settings/preferences', { data: { shortcut_keys: {} } });
	await page.goto('/dev/shortcuts');
	await page.getByRole('button', { name: 'Change the key for Next result' }).click();
	await page.keyboard.press('n');
	await page.getByRole('button', { name: 'Save' }).click();
	await expect(page.getByText('Keyboard settings saved')).toBeVisible();

	await page.reload();
	await expect(page.getByTestId('shortcut-key-results.next')).toContainText('N');

	// A clash the editor would have caught, sent straight to the server.
	const res = await page.request.patch('/api/v1/settings/preferences', {
		data: { shortcut_keys: { 'results.next': 'k' } }
	});
	expect(res.status()).toBe(422);
	const error = (await res.json()).error as { message: string };
	expect(error.message).toContain('both use k');
	void request;
});

test('the server message shows under its row', async ({ page }) => {
	await page.goto('/dev/shortcuts');
	// Another tab saves a clash-free change; this tab then tries to take the key it now holds.
	await page.request.patch('/api/v1/settings/preferences', {
		data: { shortcut_keys: { 'results.previous': 'p' } }
	});
	await page.getByRole('button', { name: 'Change the key for Next result' }).click();
	await page.keyboard.press('p');
	// The stale tab does not know about the remap, so the editor lets it through; the save is
	// refused with the revision or the clash, and either way the page says so.
	await page.getByRole('button', { name: 'Save' }).click();
	await expect(page.getByText(/both use p|Changed in another tab or device/).first()).toBeVisible();
});
