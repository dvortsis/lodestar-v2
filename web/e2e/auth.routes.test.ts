// M-auth flows against the OpenAPI mock: first-run setup, sign-in and its return to `next`, the
// refused open redirect, rate limiting, and a session that expires mid-use. axe runs on each
// screen in Riverstone light and dark.
import { expectNoA11yViolations } from './a11y';
import { body } from './search-results.fixtures';
import { expect, test } from './support/test';

const TOKEN = 'AB12-CD34-EF56-7890';
const envelope = (code: string) => ({ error: { code, message: `server text for ${code}` } });
const signedOut = { logged_in: false, setup_required: false };
const fresh = { logged_in: false, setup_required: true };

/** The path and query the page ended on, without the harness's density parameter. */
function landed(url: string) {
	const u = new URL(url);
	u.searchParams.delete('density');
	return u.pathname + u.search;
}

for (const [themeId, colorMode] of [
	['riverstone-light', 'light'],
	['riverstone-dark', 'dark']
] as const) {
	test.describe(themeId, () => {
		test.use({ themeId, colorMode });

		test('setup and sign-in screens are axe-clean, with every state shown', async ({
			page,
			go,
			mock
		}) => {
			mock.on('getAuthStatus', fresh);
			mock.on('setup', { status: 401, body: envelope('bad_token') });
			await go('/setup');
			await page.getByText('Where to find the log').click();
			await page.getByLabel('Setup token').fill('nonsense');
			await page.getByRole('button', { name: 'Continue' }).click();
			await expect(page.getByText(/doesn't look like a setup token/)).toBeVisible();
			await expectNoA11yViolations(page);
			await page.getByLabel('Setup token').fill(TOKEN);
			await page.getByRole('button', { name: 'Continue' }).click();
			await page.getByLabel('New admin password').fill('short');
			await page.getByRole('button', { name: 'Create password and sign in' }).click();
			await expect(page.getByText('Use at least 10 characters.')).toBeVisible();
			await expectNoA11yViolations(page);

			mock.on('getAuthStatus', signedOut);
			mock.on('login', {
				status: 429,
				body: envelope('rate_limited'),
				headers: { 'Retry-After': '30' }
			});
			await go('/login');
			await page.getByLabel('Password').fill('wrong wrong wrong');
			await page.getByRole('button', { name: 'Sign in' }).click();
			await expect(page.getByTestId('auth-countdown')).toBeVisible();
			await expectNoA11yViolations(page);
		});
	});
}

test('first-run setup: token from a pasted log line, password, then /welcome', async ({
	page,
	go,
	mock
}) => {
	mock.on('getAuthStatus', fresh);
	mock.on('setup', { ok: true });
	await go('/setup');
	await page
		.getByLabel('Setup token')
		.fill(
			`WARN lodestar: first run: open http://0.0.0.0:8090/ and enter setup token ${TOKEN.toLowerCase()} to create the admin password`
		);
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByLabel('New admin password')).toBeFocused();
	await page.getByLabel('New admin password').fill('correct horse battery');
	await page.getByLabel('Confirm password').fill('correct horse battery');
	await page.getByRole('button', { name: 'Create password and sign in' }).click();
	await expect(page).toHaveURL(/\/welcome(\?|$)/);
	const [call] = mock.calls('setup');
	expect(call!.postDataJSON()).toEqual({ token: TOKEN, password: 'correct horse battery' });
	expect(call!.headers()['content-type']).toBe('application/json');
	// Nothing sensitive was left in the URL or in browser storage.
	expect(page.url()).not.toContain(TOKEN);
	const stored = await page.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }));
	expect(stored).not.toContain(TOKEN);
	expect(stored).not.toContain('correct horse');
});

test('a wrong setup token returns to step 1 with a clear error, never echoing the token', async ({
	page,
	go,
	mock
}) => {
	mock.on('getAuthStatus', fresh);
	mock.on('setup', { status: 401, body: envelope('bad_token') });
	await go('/setup');
	await page.getByLabel('Setup token').fill(TOKEN);
	await page.getByRole('button', { name: 'Continue' }).click();
	await page.getByLabel('New admin password').fill('correct horse battery');
	await page.getByLabel('Confirm password').fill('correct horse battery');
	await page.getByRole('button', { name: 'Create password and sign in' }).click();
	const message = page.getByText(/That setup token doesn't match/);
	await expect(message).toBeVisible();
	await expect(page.getByLabel('Setup token')).toBeFocused();
	await expect(message).not.toContainText(TOKEN);
	await expect(page.locator('body')).not.toContainText('server text');
});

test('setup already done redirects to /login', async ({ page, go, mock }) => {
	mock.on('getAuthStatus', signedOut);
	await go('/setup');
	await expect(page).toHaveURL(/\/login$/);
	await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();
});

test('sign in returns to next', async ({ page, go, mock }) => {
	mock.on('getAuthStatus', signedOut);
	let attempts = 0;
	mock.on('login', () =>
		++attempts === 1
			? { status: 401, body: envelope('bad_password') }
			: { status: 200, body: { ok: true } }
	);
	// The search page parses the query it lands on (S-search-1).
	mock.on('parseQuery', { ast: { op: 'word', field: 'any', word: 'ubuntu' }, canonical: 'ubuntu' });
	// ...and asks for its results (S-search-2): none, so the request can't outlive the test unmocked.
	mock.on('searchPost', body([], { canonical: 'ubuntu' }));
	await go(`/login?next=${encodeURIComponent('/search?q=ubuntu&sort=size')}`);
	await page.getByLabel('Password').fill('not it at all');
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page.getByText('That password is wrong. Check it and try again.')).toBeVisible();
	await expect(page.getByLabel('Password')).toBeFocused();
	await page.getByLabel('Password').fill('correct horse battery');
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page).toHaveURL(/\/search\?/);
	expect(landed(page.url())).toBe('/search?q=ubuntu&sort=size');
	expect(mock.calls('login')).toHaveLength(2);
});

for (const evil of [
	'//evil.example/',
	'https://evil.example/',
	'/\\evil.example',
	'/\t/evil.example',
	'/.//evil.example/'
]) {
	test(`the open-redirect attempt ${JSON.stringify(evil)} is refused`, async ({
		page,
		go,
		mock
	}) => {
		mock.on('getAuthStatus', signedOut);
		mock.on('login', { ok: true });
		await go(`/login?next=${encodeURIComponent(evil)}`);
		await page.getByLabel('Password').fill('correct horse battery');
		await page.getByRole('button', { name: 'Sign in' }).click();
		// `/` redirects to /search: still this origin.
		await expect(page).toHaveURL(/\/search/);
		expect(new URL(page.url()).hostname).toBe('localhost');
	});
}

test('a signed-in visitor of /login goes straight to next', async ({ page, go, mock }) => {
	mock.on('getAuthStatus', { logged_in: true, setup_required: false });
	await go(`/login?next=${encodeURIComponent('/settings/general')}`);
	await expect(page).toHaveURL(/\/settings\/general/);
});

test('a signed-in visitor with a dot-segment next lands home, not stuck', async ({
	page,
	go,
	mock
}) => {
	mock.on('getAuthStatus', { logged_in: true, setup_required: false });
	await go(`/login?next=${encodeURIComponent('/.//evil.example/')}`);
	await expect(page).toHaveURL(/\/search/);
	expect(new URL(page.url()).hostname).toBe('localhost');
});

test('a 429 counts down from Retry-After and unlocks', async ({ page, go, mock }) => {
	await page.clock.install();
	mock.on('getAuthStatus', signedOut);
	let attempts = 0;
	mock.on('login', () =>
		++attempts === 1
			? { status: 429, body: envelope('rate_limited'), headers: { 'Retry-After': '3' } }
			: { status: 200, body: { ok: true } }
	);
	await go('/login');
	await page.getByLabel('Password').fill('correct horse battery');
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page.getByTestId('auth-countdown')).toHaveText('Try again in 3 s.');
	await expect(page.getByRole('alert')).toContainText('Too many attempts');
	// Locked: a second submit sends nothing.
	await page.getByLabel('Password').press('Enter');
	await page.clock.runFor(1000);
	await expect(page.getByTestId('auth-countdown')).toHaveText('Try again in 2 s.');
	await page.clock.runFor(2000);
	await expect(page.getByText('You can try again now.')).toBeVisible();
	expect(attempts).toBe(1);
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page).toHaveURL(/\/search/);
});

test('a session that expires mid-use goes to /login?next=… and back', async ({ page, mock }) => {
	let session = false;
	mock.on('getAuthStatus', () => ({
		status: 200,
		body: { logged_in: session, setup_required: false }
	}));
	mock.on('getFilters', () =>
		session
			? { status: 200, body: {} }
			: {
					status: 401,
					body: { ...envelope('login_required'), setup_required: false }
				}
	);
	mock.on('login', () => {
		session = true;
		return { status: 200, body: { ok: true } };
	});
	await page.goto('/dev/auth?q=ubuntu');
	await expect(page).toHaveURL(/\/login\?next=/);
	expect(new URL(page.url()).searchParams.get('next')).toBe('/dev/auth?q=ubuntu');
	await page.getByLabel('Password').fill('correct horse battery');
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page).toHaveURL(/\/dev\/auth\?q=ubuntu$/);
	await expect(page.getByText('Loaded with a session')).toBeVisible();
});

test('setup_required on a 401 goes to /setup instead', async ({ page, mock }) => {
	mock.on('getAuthStatus', fresh);
	mock.on('getFilters', {
		status: 401,
		body: { ...envelope('login_required'), setup_required: true }
	});
	await page.goto('/dev/auth');
	await expect(page).toHaveURL(/\/setup$/);
	await expect(page.getByRole('heading', { name: 'Set up Lodestar' })).toBeVisible();
});
