// X-auth-early-401: a signed-out visitor's first requests fail before the shell installs its
// lazy 401 handler. The first such 401 is kept and replayed once the handler is in, so the page
// goes to sign-in, returning here, instead of staying broken. No interaction: each page is only
// loaded. (The /dev/auth harness installs the handlers first, so it can't show this.)
import type { Page } from '@playwright/test';
import { expect, test } from './support/test';

const WAIT = { timeout: 10_000 };

/**
 * The layout's boot check (GET /auth on the first load, X-setup-token-boot) fails once, so the
 * page loads and its own requests meet the 401: the hand-off this spec is about. It still matters
 * when that check fails or times out. The sign-in page's own GET /auth gets the mock's answer.
 */
const bootCheckFails = (page: Page) =>
	page.route('**/api/v1/auth', (route) => route.fulfill({ status: 503 }), { times: 1 });

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
		await bootCheckFails(page);
		await page.goto(path);
		await expect(page).toHaveURL(/\/login\?/, WAIT);
		expect(new URL(page.url()).searchParams.get('next')).toBe(path);
	});
}
