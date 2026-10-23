// M-filters-dialogs against the real server (`lodestar serve --fixture`), opt-in: it runs only when
// E2E_FIXTURE_URL (the server, built with VITE_DEV_ROUTES=1 so /dev/filters-flows exists) and
// E2E_FIXTURE_PASSWORD (its LODESTAR_FIXTURE_PASSWORD) are set. It imports @playwright/test
// directly: no OpenAPI mock, every answer is the server's. One wrong password in all, well under
// the login bucket's burst of 5.
import { expect, test, type Page } from '@playwright/test';

const URL = process.env.E2E_FIXTURE_URL;
const PASSWORD = process.env.E2E_FIXTURE_PASSWORD;

test.skip(
	!URL || !PASSWORD,
	'set E2E_FIXTURE_URL and E2E_FIXTURE_PASSWORD to run against a fixture'
);
test.use({ baseURL: URL });
test.describe.configure({ mode: 'serial' });

/** A same-origin JSON request from the page, as the app makes them. */
const call = (page: Page, method: string, path: string, body: unknown) =>
	page.evaluate(
		async ([method, path, body]) => {
			const r = await fetch(`/api/v1${path}`, {
				method,
				headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
				body: JSON.stringify(body),
				credentials: 'same-origin'
			});
			return { status: r.status, body: await r.json().catch(() => null) };
		},
		[method, path, body] as const
	);

async function signIn(page: Page) {
	await page.goto('/login');
	await page.getByLabel('Password').fill(PASSWORD!);
	await page.getByRole('button', { name: 'Sign in' }).click();
	await page.waitForURL((u) => !u.pathname.startsWith('/login'));
	await page.goto('/dev/filters-flows');
	await expect(page.getByRole('heading', { name: 'Filter change flows' })).toBeVisible();
}

test('switching off a non-floor core rule: warning, 428, 401, apply; on again asks nothing', async ({
	page
}) => {
	await signIn(page);
	const off = { op: 'core_switch', rule: 'CS-4a', on: false };
	expect((await call(page, 'POST', '/filters/apply', { change: off })).status).toBe(428);

	await page.getByTestId('case-core-off').click();
	await expect(page.getByTestId('effect-warning-text')).toContainText('CS-4a');
	const field = page.getByLabel('Password');
	await expect(field).toBeFocused();
	await field.fill('not-the-password');
	await field.press('Enter');
	await expect(page.getByTestId('field-error')).toContainText('wrong');
	await field.fill(PASSWORD!);
	await field.press('Enter');
	await expect(page.getByText('What changed')).toBeVisible();
	await page.getByTestId('change-cancel').click();

	await page.getByTestId('case-core-on').click();
	await expect(page.getByTestId('change-dialog')).toHaveAttribute('data-state', 'review');
	await expect(page.locator('input[name="password"]')).toHaveCount(0);
	await page.getByTestId('change-apply').click();
	await expect(page.getByText('What changed')).toBeVisible();
});

test('switching off a floor rule is refused, and the dialog says so', async ({ page }) => {
	await signIn(page);
	await page.getByTestId('case-floor-off').click();
	await expect(page.getByTestId('change-dialog')).toHaveAttribute('data-state', 'failed');
	await expect(page.getByTestId('change-error')).not.toBeEmpty();
	await expect(page.locator('input[name="password"]')).toHaveCount(0);
});

test('Show adult content needs the 18+ confirmation, not a password', async ({ page }) => {
	await signIn(page);
	const on = await call(page, 'PATCH', '/settings/content', { show_adult: true });
	expect([on.status, on.body?.error?.code]).toEqual([422, 'adult_confirmation_required']);
	await page.getByTestId('adult-on').click();
	await page.getByTestId('adult-show').click();
	await expect(page.getByTestId('adult-status')).toHaveText('Adult content shown');
	await page.getByTestId('adult-off').click();
	await expect(page.getByTestId('adult-status')).toHaveText('Adult content hidden');
});
