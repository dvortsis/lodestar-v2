// S-detail against the real binary (`lodestar serve --fixture`), local only: skipped unless
// E2E_FIXTURE_URL is set. Expected hashes come from the fixture's own manifest, never from code.
//
//   E2E_FIXTURE_URL=http://127.0.0.1:8291 E2E_FIXTURE_DIR=/tmp/lodestar-fixture \
//   E2E_FIXTURE_PASSWORD='correct horse battery' npx playwright test -c <config> detail.fixture
//
// It uses Playwright's own `test` (no OpenAPI mock) and the real server's CSP, so the tree worker
// is checked under `script-src 'self'`.
import { readFileSync } from 'node:fs';
import { expect, test, type BrowserContext } from '@playwright/test';

const URL_ = process.env.E2E_FIXTURE_URL;
const DIR = process.env.E2E_FIXTURE_DIR ?? '';
const PASSWORD = process.env.E2E_FIXTURE_PASSWORD ?? '';

interface Entry {
	key: string;
	hash: string;
	name: string;
	files: number;
	content_type: string;
	swarm: null | { seeders: number; trackers_checked: number; trackers_reporting: number };
}

test.skip(!URL_, 'E2E_FIXTURE_URL is not set');
test.use({ baseURL: URL_ });

const manifest = () =>
	JSON.parse(readFileSync(`${DIR}/fixture.json`, 'utf8')) as { torrents: Entry[] };

// One sign-in for the file: the server rate-limits repeated logins.
let cookies: Awaited<ReturnType<BrowserContext['cookies']>> = [];
test.beforeAll(async ({ playwright }) => {
	const api = await playwright.request.newContext({ baseURL: URL_ });
	const res = await api.post('/api/v1/login', { data: { password: PASSWORD } });
	expect(res.ok(), 'sign in to the fixture').toBe(true);
	cookies = (await api.storageState()).cookies;
	await api.dispose();
});
test.beforeEach(async ({ context }) => {
	await context.addCookies(cookies);
});

test("the 1,200-file pack loads every page (main-thread path, under the server's CSP)", async ({
	page
}) => {
	const pack = manifest().torrents.find((t) => t.name.startsWith('Field Recordings Archive'))!;
	const csp: string[] = [];
	page.on('console', (m) => {
		if (/Content Security Policy|Refused to/.test(m.text())) csp.push(m.text());
	});
	await page.goto(`/t/${pack.hash}`);
	const tree = page.getByTestId('detail-file-tree');
	await expect(tree).toHaveAttribute('aria-busy', 'false', { timeout: 20_000 });
	await expect(page.getByTestId('detail-files')).toHaveText(
		new Intl.NumberFormat('en').format(pack.files)
	);
	// 1,200 is under the worker threshold: the main-thread path
	await expect(tree).toHaveAttribute('data-tree-worker', 'false');
	await page.getByRole('searchbox', { name: 'Filter files' }).fill('.');
	await expect(page.getByRole('tree', { name: 'Files in this torrent' })).toBeVisible();
	expect(csp).toEqual([]);
});

test('a Lantern Keeper version shows its composition', async ({ page }) => {
	const lantern = manifest().torrents.find((t) => t.name.includes('Lantern Keeper'))!;
	await page.goto(`/t/${lantern.hash}`);
	await expect(page.getByTestId('detail-compbar-dominant')).toBeVisible();
	await expect(page.getByTestId('detail-groups-item').first()).toBeVisible();
});

test('a never-scraped torrent says so', async ({ page }) => {
	const never = manifest().torrents.find((t) => t.swarm === null && t.content_type !== 'adult')!;
	await page.goto(`/t/${never.hash}`);
	await expect(page.getByTestId('detail-scrape-age')).toHaveText('Never scraped');
	await expect(page.getByTestId('detail-seeders')).toContainText('Never scraped');
});

test('an Adult torrent with Adult hidden gives the adult_hidden state', async ({ page }) => {
	const adult = manifest().torrents.find((t) => t.content_type === 'adult')!;
	await page.goto(`/t/${adult.hash}`);
	await expect(page.getByTestId('detail-adult-hidden')).toBeVisible();
});

test("the server's CSP allows a same-origin module worker and has no blob: source", async ({
	page
}) => {
	// The fixture has no torrent above the 2,000-file worker threshold, so check the policy the
	// worker runs under: script-src 'self' (worker-src falls back to it), and a module worker from a
	// same-origin script starts without a violation.
	const res = await page.goto('/search');
	const csp = res!.headers()['content-security-policy'] ?? '';
	expect(csp).toContain("script-src 'self'");
	expect(csp).not.toContain('blob:');
	const result = await page.evaluate(async () => {
		const violations: string[] = [];
		document.addEventListener('securitypolicyviolation', (e) =>
			violations.push(e.violatedDirective)
		);
		const src = document.querySelector<HTMLLinkElement>('link[rel="modulepreload"]')?.href;
		if (!src) return { started: false, violations };
		const worker = new Worker(src, { type: 'module' });
		await new Promise((r) => setTimeout(r, 500));
		worker.terminate();
		return { started: true, violations };
	});
	expect(result.violations).toEqual([]);
});
