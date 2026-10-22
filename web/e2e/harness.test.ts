// Self-tests of the harness: the OpenAPI mock, the theme rotation, and the keyboard-only guard.
import { loadOperations } from './mock/openapi-mock';
import { rotationSeed, sampleThemes, OTHER_THEMES, ALL_THEMES } from './support/themes';
import { expect, test } from './support/test';

const fetchJson = (page: import('@playwright/test').Page, url: string, init?: RequestInit) =>
	page.evaluate(
		async ([u, i]) => {
			const r = await fetch(u as string, i as RequestInit);
			return { status: r.status, body: await r.json().catch(() => null) };
		},
		[url, init ?? {}] as const
	);

test.describe('OpenAPI mock', () => {
	test.beforeEach(async ({ page }) => {
		await page.goto('/search');
	});

	test('answers an operation with a typed override', async ({ page, mock }) => {
		mock.on('getAuthStatus', { authenticated: true, setup_required: false } as never);
		const res = await fetchJson(page, '/api/v1/auth');
		expect(res.status).toBe(200);
		expect(res.body).toEqual({ authenticated: true, setup_required: false });
		expect(mock.calls('getAuthStatus')).toHaveLength(1);
	});

	test('matches path templates, preferring static segments', async ({ page, mock }) => {
		mock
			.on('getFilterQueue', { held: true } as never)
			.on('getFilterPreviewJob', { id: 'x' } as never);
		expect((await fetchJson(page, '/api/v1/filters/queue')).body).toEqual({ held: true });
		expect((await fetchJson(page, '/api/v1/filters/preview/jobs/abc')).body).toEqual({ id: 'x' });
	});

	test('error responses use the shared envelope', async ({ page, mock }) => {
		mock.error('searchGet', 400, 'unknown_sort', 'Unknown sort', { field: 'sort' });
		const res = await fetchJson(page, '/api/v1/search?sort=nope');
		expect(res).toEqual({
			status: 400,
			body: { error: { code: 'unknown_sort', message: 'Unknown sort', field: 'sort' } }
		});
	});

	test('overrides may be functions of the request', async ({ page, mock }) => {
		mock.on(
			'parseQuery',
			(request) => ({ echoed: new URL(request.url()).searchParams.get('q') }) as never
		);
		expect((await fetchJson(page, '/api/v1/parse?q=ubuntu')).body).toEqual({ echoed: 'ubuntu' });
	});

	test('an operation with no example and no override fails the test, naming it', async ({
		page,
		mock
	}) => {
		const res = await fetchJson(page, '/api/v1/classify');
		expect(res.status).toBe(501);
		expect(res.body.error.code).toBe('mock_no_example');
		expect(() => mock.assertNoFailures()).toThrow(/getClassify.*no documented example/);
		mock['failures'].length = 0; // consumed: the teardown must not fail this test
	});

	test('an unmatched request fails the test, naming the request', async ({ page, mock }) => {
		const res = await fetchJson(page, '/api/v1/nope');
		expect(res.status).toBe(501);
		expect(() => mock.assertNoFailures()).toThrow(/GET \/api\/v1\/nope matches no operation/);
		mock['failures'].length = 0;
	});

	test('a wrong method does not match the path', async ({ page, mock }) => {
		await fetchJson(page, '/api/v1/auth', { method: 'DELETE' });
		expect(() => mock.assertNoFailures()).toThrow(/DELETE \/api\/v1\/auth/);
		mock['failures'].length = 0;
	});

	test('mock.on rejects an unknown operation', ({ mock }) => {
		expect(() => mock.on('nope' as never, {} as never)).toThrow(/no operation "nope"/);
	});

	test('every operation id is unique', () => {
		const ids = loadOperations().map((o) => o.id);
		expect(new Set(ids).size).toBe(ids.length);
	});
});

test.describe('theme rotation', () => {
	test('is deterministic and picks 6 distinct non-Riverstone themes', () => {
		const a = sampleThemes(42);
		expect(a).toEqual(sampleThemes(42));
		expect(new Set(a.map((t) => t.id)).size).toBe(6);
		expect(a.every((t) => !t.id.startsWith('riverstone-'))).toBe(true);
	});

	test('eight consecutive runs cover every non-Riverstone theme', () => {
		const seen = new Set<string>();
		for (let run = 100; run < 108; run++) sampleThemes(run).forEach((t) => seen.add(t.id));
		expect(seen.size).toBe(OTHER_THEMES.length);
	});

	test('there are 48 themes: 24 families × light/dark', () => {
		expect(ALL_THEMES).toHaveLength(48);
	});

	test('the seed is the CI run number, else the day', () => {
		expect(rotationSeed({ GITHUB_RUN_NUMBER: '17' } as never)).toBe(17);
		expect(rotationSeed({} as never)).toBeGreaterThan(20000);
	});
});

test.describe('keyboard-only guard', () => {
	test('keyboard activation passes; page.mouse throws', async ({ page, kbd }) => {
		await page.goto('/search');
		await page.evaluate(() => {
			document.body.innerHTML = '<button id=a onclick="window.hit=1">Go</button>';
		});
		await kbd.tabTo(page.locator('#a'));
		await kbd.press('Enter');
		expect(await page.evaluate(() => (window as never as { hit: number }).hit)).toBe(1);
		expect(await kbd.pointerEvents()).toEqual([]);
		expect(() => page.mouse.click(1, 1)).toThrow(/page\.mouse\.click/);
	});

	test('a pointer click is recorded', async ({ page, kbd }) => {
		await page.goto('/search');
		await page.evaluate(() => {
			document.body.innerHTML = '<button id=a>Go</button>';
		});
		await page.locator('#a').click(); // locator clicks go through the real mouse path
		expect((await kbd.pointerEvents()).length).toBeGreaterThan(0);
		await page.evaluate(() => (window.__pointerUsed = [])); // consumed: teardown must pass
	});
});
