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

test("the real tree worker starts under the server's CSP and answers a build", async ({ page }) => {
	// The fixture has no torrent above the 2,000-file worker threshold, so start the shipped worker
	// directly: find its URL in the tree chunk the page loads, then post it a small build.
	const pack = manifest().torrents.find((t) => t.name.startsWith('Field Recordings Archive'))!;
	const res = await page.goto(`/t/${pack.hash}`);
	const csp = res!.headers()['content-security-policy'] ?? '';
	expect(csp).toContain("script-src 'self'");
	expect(csp).not.toContain('blob:');
	await expect(page.getByTestId('detail-file-tree')).toHaveAttribute('aria-busy', 'false', {
		timeout: 20_000
	});
	const outcome = await page.evaluate(async () => {
		const scripts = performance
			.getEntriesByType('resource')
			.map((e) => e.name)
			.filter((u) => u.endsWith('.js'));
		let workerUrl: string | undefined;
		for (const u of scripts) {
			const m = (await (await fetch(u)).text()).match(/workers\/tree\.worker-[\w-]+\.js/);
			if (m) {
				workerUrl = new URL('/_app/immutable/' + m[0], location.origin).href;
				break;
			}
		}
		if (!workerUrl) return { error: 'no worker url found' };
		return await new Promise<{ error?: string; nodes?: number }>((resolve) => {
			const violation = (e: SecurityPolicyViolationEvent) =>
				resolve({ error: e.violatedDirective });
			document.addEventListener('securitypolicyviolation', violation);
			// A deadline, not a pause: resolves the moment the worker answers.
			const deadline = setTimeout(() => resolve({ error: 'no answer in 10 s' }), 10_000);
			const worker = new Worker(workerUrl!, { type: 'module' });
			worker.onerror = (e) => resolve({ error: String(e.message || 'worker error') });
			worker.onmessage = (e) => {
				clearTimeout(deadline);
				resolve({ nodes: e.data.nodes.length });
			};
			worker.postMessage({ type: 'append', files: [{ path: 'a/b.txt', size: 1, attrs: [] }] });
			worker.postMessage({
				type: 'build',
				seq: 1,
				options: {
					showPadding: false,
					filter: '',
					matched: [],
					locale: 'en',
					labels: { executable: 'x', symlink: 'y' }
				}
			});
		});
	});
	expect(outcome).toEqual({ nodes: 2 });
});
