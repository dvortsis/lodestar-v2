// One sign-in for the whole `fixture` project. The server rate-limits logins (a burst of 5 per
// address, then one every 12 s) and the limiter is left as it is, so the specs share the session
// this global setup stores under test-results/ (gitignored) instead of each logging in.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { request } from '@playwright/test';

type Cookie = Parameters<import('@playwright/test').BrowserContext['addCookies']>[0][number];

/** Where the signed-in storage state is written; a path a spec can pass as `storageState`. */
// Absolute, next to the config, so it does not depend on the working folder (Playwright clears
// test-results before the global setup runs, so a stale session is never reused).
export const FIXTURE_STATE = fileURLToPath(
	new URL('../../test-results/.auth/fixture.json', import.meta.url)
);

/** The stored session's cookies, for a spec that needs them on a context it made itself. */
export const fixtureCookies = (): Cookie[] =>
	JSON.parse(readFileSync(FIXTURE_STATE, 'utf8')).cookies as Cookie[];

export default async function globalSetup() {
	const url = process.env.E2E_FIXTURE_URL;
	const password = process.env.E2E_FIXTURE_PASSWORD ?? process.env.LODESTAR_FIXTURE_PASSWORD;
	// No server named: the specs skip themselves, as they always have.
	if (!url) return;
	if (!password)
		throw new Error(
			'E2E_FIXTURE_URL is set but not E2E_FIXTURE_PASSWORD (or LODESTAR_FIXTURE_PASSWORD): the fixture project cannot sign in'
		);
	const api = await request.newContext({ baseURL: url });
	try {
		// Wait for the server (a condition, not a fixed time): up to 60 s.
		const deadline = Date.now() + 60_000;
		for (;;) {
			const ok = await api
				.get('/healthz')
				.then((r) => r.ok())
				.catch(() => false);
			if (ok) break;
			if (Date.now() > deadline) throw new Error(`fixture server at ${url} not healthy in 60 s`);
			await new Promise((r) => setTimeout(r, 500));
		}
		const res = await api.post('/api/v1/login', { data: { password } });
		if (!res.ok())
			throw new Error(
				`fixture sign-in failed: HTTP ${res.status()} from ${url}/api/v1/login (wrong E2E_FIXTURE_PASSWORD, or the login limiter is still cooling down)`
			);
		mkdirSync(dirname(FIXTURE_STATE), { recursive: true });
		writeFileSync(FIXTURE_STATE, JSON.stringify(await api.storageState()), { mode: 0o600 });
	} finally {
		await api.dispose();
	}
}
