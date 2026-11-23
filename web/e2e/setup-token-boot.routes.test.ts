// X-setup-token-boot (UI review I13, U3, I14): the setup token is checked on Continue, before the
// password step, sharing setup's attempt limit; and one session check at boot sends a fresh
// instance to setup and a signed-out visitor to sign-in before any other page renders.
import { expect, test } from './support/test';

const TOKEN = 'AB12-CD34-EF56-7890';
const envelope = (code: string) => ({ error: { code, message: `server text for ${code}` } });
const status = (logged_in: boolean, notice_required: boolean, setup_required: boolean) => ({
	logged_in,
	notice_required,
	setup_required
});

async function toTokenStep(page: import('@playwright/test').Page) {
	await page.getByLabel('I have read the Legal Notice').check();
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByLabel('Setup token')).toBeFocused();
}

test('a wrong setup token is refused on Continue, before the password', async ({
	page,
	go,
	mock
}) => {
	mock.on('getAuthStatus', status(false, false, true));
	mock.on('checkSetupToken', (request) =>
		request.postDataJSON().token === TOKEN
			? { status: 200, body: { ok: true } }
			: { status: 401, body: envelope('bad_token') }
	);
	await go('/setup');
	await toTokenStep(page);
	await page.getByLabel('Setup token').fill('0000-0000-0000-0000');
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByText(/That setup token doesn't match/)).toBeVisible();
	await expect(page.getByLabel('Setup token')).toBeFocused();
	await expect(page.getByLabel('New admin password')).toHaveCount(0);
	await expect(page.getByText('server text for bad_token')).toHaveCount(0);
	expect(mock.calls('setup')).toHaveLength(0);
	// The right one goes on, and isn't asked about again after Back.
	await page.getByLabel('Setup token').fill(TOKEN);
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByLabel('New admin password')).toBeFocused();
	await page.getByRole('button', { name: 'Back' }).click();
	await expect(page.getByLabel('Setup token')).toBeFocused();
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByLabel('New admin password')).toBeFocused();
	expect(mock.calls('checkSetupToken')).toHaveLength(2);
});

test('Continue counts down when the attempts are used up', async ({ page, go, mock }) => {
	mock.on('getAuthStatus', status(false, false, true));
	mock.on('checkSetupToken', {
		status: 429,
		body: envelope('rate_limited'),
		headers: { 'Retry-After': '2' }
	});
	await go('/setup');
	await toTokenStep(page);
	await page.getByLabel('Setup token').fill(TOKEN);
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByTestId('auth-countdown')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Continue' })).toHaveAttribute(
		'aria-disabled',
		'true'
	);
	await expect(page.getByLabel('New admin password')).toHaveCount(0);
	await expect(page.getByText(/You can try again now/)).toBeVisible({ timeout: 10_000 });
});

// The page never renders: it would ask for its own data, and the mock fails the test at teardown
// for any request it can't answer (these pages' data has no default here).
for (const path of ['/alerts?x=1', '/settings/notifications', '/insights']) {
	test(`a fresh instance opened at ${path} lands on setup without loading the page`, async ({
		page,
		mock
	}) => {
		mock.on('getAuthStatus', status(false, false, true));
		await page.goto(path);
		await expect(page).toHaveURL(/\/setup$/);
		await expect(page.getByRole('region', { name: 'Legal Notice' })).toBeVisible();
		// The boot check, then the setup page's own.
		expect(mock.calls('getAuthStatus')).toHaveLength(2);
	});
}

test('a signed-out visitor at /search goes to sign-in and returns after it', async ({
	page,
	mock
}) => {
	let signedIn = false;
	mock.on('getAuthStatus', () => ({ status: 200, body: status(signedIn, false, false) }));
	mock.on('login', () => {
		signedIn = true;
		return { status: 200, body: { ok: true } };
	});
	await page.goto('/search');
	await expect(page).toHaveURL(/\/login\?next=%2Fsearch$/);
	await expect(page.getByRole('search')).toHaveCount(0);
	await page.getByLabel('Password').fill('correct horse battery');
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page).toHaveURL(/\/search$/);
	await expect(page.getByRole('search')).toBeVisible();
});

test('a server that answers GET /auth with an error leaves the page as it was', async ({
	page,
	mock
}) => {
	mock.on('getAuthStatus', { status: 503, body: envelope('unavailable') });
	await page.goto('/search');
	await expect(page.getByRole('search')).toBeVisible();
	await expect(page).toHaveURL(/\/search$/);
});
