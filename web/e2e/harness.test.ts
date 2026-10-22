// Self-tests of the harness: the OpenAPI mock, the theme rotation, and the keyboard-only guard.
import { loadOperations, OpenApiMock, type OperationId, type Spec } from './mock/openapi-mock';
import { fullPageAt, snapshotName } from './support/policy';
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
		mock.on('getAuthStatus', { logged_in: true, setup_required: false });
		const res = await fetchJson(page, '/api/v1/auth');
		expect(res.status).toBe(200);
		expect(res.body).toEqual({ logged_in: true, setup_required: false });
		expect(mock.calls('getAuthStatus')).toHaveLength(1);
	});

	test('matches path templates, preferring static segments', async ({ page, mock }) => {
		mock
			.on('getFilterQueue', { decisions_30d: [], quarantine: [] })
			.on('getFilterPreviewJob', { status: 200, body: { id: 'x' } });
		expect((await fetchJson(page, '/api/v1/filters/queue')).body).toEqual({
			decisions_30d: [],
			quarantine: []
		});
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
		mock.on('parseQuery', (request) => ({
			status: 200,
			body: { echoed: new URL(request.url()).searchParams.get('q') }
		}));
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
		expect(() => mock.on('nope' as OperationId, {})).toThrow(/no operation "nope"/);
	});

	test('every operation id is unique', () => {
		const ids = loadOperations().map((o) => o.id);
		expect(new Set(ids).size).toBe(ids.length);
	});
});

// openapi.json has no examples yet (a B1 follow-up), so the example path is tested on a synthetic
// spec covering every place a documented example can live.
test.describe('OpenAPI mock: documented examples', () => {
	const json = (extra: Record<string, unknown>) => ({
		description: '',
		content: { 'application/json': extra }
	});
	const spec: Spec = {
		servers: [{ url: '/api/v1' }],
		paths: {
			'/inline': {
				get: { operationId: 'inline', responses: { '200': json({ example: { a: 1 } }) } }
			},
			'/named': {
				get: {
					operationId: 'named',
					responses: {
						'200': json({ examples: { first: { value: { b: 2 } }, second: { value: {} } } })
					}
				}
			},
			'/schema': {
				get: {
					operationId: 'fromSchema',
					responses: { '200': json({ schema: { $ref: '#/components/schemas/Thing' } }) }
				}
			},
			'/shared': {
				get: {
					operationId: 'sharedResponse',
					responses: { '200': { $ref: '#/components/responses/Ok' } }
				}
			},
			'/jobs': {
				post: { operationId: 'startJob', responses: { '202': json({ example: { id: 'j1' } }) } }
			},
			'/things/queue': {
				get: { operationId: 'queue', responses: { '200': json({ example: { kind: 'queue' } }) } }
			},
			'/things/{id}': {
				get: { operationId: 'thing', responses: { '200': json({ example: { kind: 'thing' } }) } }
			},
			'/bare': {
				get: {
					operationId: 'bare',
					responses: { '200': json({ schema: { type: 'object' } }), default: {} }
				}
			}
		},
		components: {
			schemas: { Thing: { type: 'object', example: { c: 3 } } },
			responses: { Ok: json({ example: { shared: true } }) }
		}
	};
	const ops = loadOperations(spec);
	const example = (id: string) => ops.find((o) => o.id === id)?.example;

	test('finds an example in each supported place', () => {
		expect(example('inline')).toEqual({ status: 200, body: { a: 1 } });
		expect(example('named')).toEqual({ status: 200, body: { b: 2 } });
		expect(example('fromSchema')).toEqual({ status: 200, body: { c: 3 } });
		expect(example('sharedResponse')).toEqual({ status: 200, body: { shared: true } });
	});

	test('keeps the documented 2xx status (202)', () => {
		expect(example('startJob')).toEqual({ status: 202, body: { id: 'j1' } });
	});

	test('an operation without an example has none', () => {
		expect(example('bare')).toBeNull();
	});

	test('static segments are tried before parameters', () => {
		const hit = (path: string) => ops.find((o) => o.method === 'GET' && o.regex.test(path))?.id;
		expect(hit('/api/v1/things/queue')).toBe('queue');
		expect(hit('/api/v1/things/42')).toBe('thing');
	});

	test('the mock replays the documented examples end to end', async ({ page, mock }) => {
		const synthetic = new OpenApiMock(ops);
		await synthetic.install(page);
		await page.goto('/search');
		expect(await fetchJson(page, '/api/v1/inline')).toEqual({ status: 200, body: { a: 1 } });
		expect(await fetchJson(page, '/api/v1/jobs', { method: 'POST' })).toEqual({
			status: 202,
			body: { id: 'j1' }
		});
		expect((await fetchJson(page, '/api/v1/things/queue')).body).toEqual({ kind: 'queue' });
		expect(() => mock.assertNoFailures()).not.toThrow();
	});
});

test.describe('snapshot policy', () => {
	const key = { name: 'k', key: true as const };
	const dense = { name: 'd', key: true as const, densityAware: true as const };
	const plain = { name: 'p' };
	const m = (density: 'comfortable' | 'compact') => ({
		shots: 'matrix' as const,
		density,
		themeId: 'riverstone-dark'
	});

	test('matrix: key routes in Comfortable, only density-aware routes in Compact', () => {
		expect(snapshotName(key, m('comfortable'))).toBe('k');
		expect(snapshotName(plain, m('comfortable'))).toBeNull();
		expect(snapshotName(dense, m('compact'))).toBe('d');
		expect(snapshotName(key, m('compact'))).toBeNull();
	});

	test('themes: key routes, named by theme; Riverstone belongs to the matrix', () => {
		const t = (themeId: string) => ({
			shots: 'themes' as const,
			density: 'comfortable' as const,
			themeId
		});
		expect(snapshotName(key, t('nord-dark'))).toBe('k--nord-dark');
		expect(snapshotName(plain, t('nord-dark'))).toBeNull();
		expect(snapshotName(key, t('riverstone-dark'))).toBeNull();
	});

	test('none: never', () => {
		expect(snapshotName(dense, { shots: 'none', density: 'comfortable', themeId: 'x' })).toBeNull();
	});

	test('full-page shots only below 1920 px', () => {
		expect(fullPageAt(1366)).toBe(true);
		expect(fullPageAt(1920)).toBe(false);
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
