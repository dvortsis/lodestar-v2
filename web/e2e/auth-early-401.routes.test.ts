// X-auth-early-401: a signed-out visitor's first requests fail before the shell installs its
// lazy 401 handler. The first such 401 is kept and replayed once the handler is in, so the page
// goes to sign-in, returning here, instead of staying broken. No interaction: each page is only
// loaded. (The /dev/auth harness installs the handlers first, so it can't show this.)
import { expect, test } from './support/test';

const WAIT = { timeout: 10_000 };

for (const path of ['/alerts?x=1', '/settings/notifications', '/search?q=ubuntu']) {
	test(`signed out, ${path} goes to sign-in and names itself as next`, async ({ page, mock }) => {
		mock.on('getAuthStatus', { logged_in: false, notice_required: false, setup_required: false });
		// Every other API call finds no session. Registered after the mock, so it's asked first.
		await page.route('**/api/v1/**', (route) =>
			new URL(route.request().url()).pathname.endsWith('/auth')
				? route.fallback()
				: route.fulfill({
						status: 401,
						contentType: 'application/json',
						body: JSON.stringify({
							error: { code: 'login_required', message: 'Log in first.' },
							setup_required: false
						})
					})
		);
		await page.goto(path);
		await expect(page).toHaveURL(/\/login\?/, WAIT);
		expect(new URL(page.url()).searchParams.get('next')).toBe(path);
	});
}
