import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	ApiError,
	api,
	isAbort,
	latest,
	mustRestartPaging,
	onLoginRequired,
	parseRetryAfter
} from './client';

const json = (body: unknown, init: ResponseInit = {}) =>
	new Response(JSON.stringify(body), {
		headers: { 'Content-Type': 'application/json' },
		...init
	});
const envelope = (code: string, status: number, extra: object = {}, headers = {}) =>
	json({ error: { code, message: `msg ${code}`, ...extra } }, { status, headers });

let fetchMock: ReturnType<typeof vi.fn>;
beforeEach(() => {
	fetchMock = vi.fn();
	vi.stubGlobal('fetch', fetchMock);
	onLoginRequired(undefined);
});
afterEach(() => {
	vi.unstubAllGlobals();
	vi.useRealTimers();
});

describe('requests', () => {
	it('builds the URL, sends cookies and returns the body', async () => {
		fetchMock.mockImplementation(async () => json({ canonical: 'a', ast: { op: 'all' } }));
		const r = await api.get('/parse', { query: { q: 'a b' } });
		expect(r.canonical).toBe('a');
		const [url, init] = fetchMock.mock.calls[0]!;
		expect(url).toBe('/api/v1/parse?q=a+b');
		expect(init.credentials).toBe('same-origin');
		expect(init.headers['Content-Type']).toBeUndefined();
	});

	it('fills path params and drops undefined query values', async () => {
		fetchMock.mockImplementation(async () => json({}));
		await api.get('/torrents/{hash}', { path: { hash: 'a/b' } });
		expect(fetchMock.mock.calls[0]![0]).toBe('/api/v1/torrents/a%2Fb');
		await api.get('/search', { query: { q: 'x', sort: undefined } });
		expect(fetchMock.mock.calls[1]![0]).toBe('/api/v1/search?q=x');
	});

	it('writes send a JSON content type and body', async () => {
		fetchMock.mockImplementation(async () => json({}));
		await api.post('/search', {
			body: {
				q: 'x',
				ast: null,
				cursor: null,
				exact: null,
				hidden: null,
				limit: null,
				offset: null,
				sort: null
			}
		});
		const [, init] = fetchMock.mock.calls[0]!;
		expect(init.method).toBe('POST');
		expect(init.headers['Content-Type']).toBe('application/json');
		expect(JSON.parse(init.body).q).toBe('x');
	});
});

describe('errors', () => {
	it('parses the envelope', async () => {
		fetchMock.mockImplementation(async () =>
			envelope('query_syntax', 400, { field: 'q', start: 2, end: 5 })
		);
		const e = await api.get('/parse', { query: { q: '(' } }).catch((x) => x);
		expect(e).toBeInstanceOf(ApiError);
		expect(e).toMatchObject({
			status: 400,
			code: 'query_syntax',
			message: 'msg query_syntax',
			field: 'q',
			start: 2,
			end: 5
		});
	});

	it('maps a non-JSON error body to http_<status>', async () => {
		fetchMock.mockImplementation(
			async () => new Response('<html>bad gateway</html>', { status: 502 })
		);
		const e = await api.get('/parse').catch((x) => x);
		expect(e).toMatchObject({ status: 502, code: 'http_502' });
	});

	it('maps a JSON body without the envelope to http_<status>', async () => {
		fetchMock.mockImplementation(async () => json({ nope: 1 }, { status: 500 }));
		expect(await api.get('/parse').catch((x) => x)).toMatchObject({ code: 'http_500' });
	});

	it('maps a network failure to code network', async () => {
		fetchMock.mockRejectedValue(new TypeError('Failed to fetch'));
		const e = await api.get('/parse').catch((x) => x);
		expect(e).toMatchObject({ status: 0, code: 'network' });
		expect(isAbort(e)).toBe(false);
	});

	it('maps an abort to an AbortError', async () => {
		const ctl = new AbortController();
		fetchMock.mockImplementation(
			(_u: string, init: RequestInit) =>
				new Promise((_, reject) =>
					init.signal!.addEventListener('abort', () => reject(new DOMException('x', 'AbortError')))
				)
		);
		const p = api.get('/parse', { signal: ctl.signal }).catch((x) => x);
		ctl.abort();
		expect(isAbort(await p)).toBe(true);
	});

	it('429 carries retryAfter in seconds', async () => {
		fetchMock.mockImplementation(async () =>
			envelope('too_many_streams', 429, {}, { 'Retry-After': '7' })
		);
		expect(await api.get('/parse').catch((x) => x)).toMatchObject({ status: 429, retryAfter: 7 });
	});

	it('rejects Retry-After values that are neither seconds nor an HTTP date', () => {
		expect(parseRetryAfter('-1')).toBeUndefined();
		expect(parseRetryAfter('1.5')).toBeUndefined();
	});

	it('keeps the envelope extras on the error', async () => {
		fetchMock.mockImplementation(async () =>
			json(
				{
					error: { code: 'login_required', message: 'x' },
					setup_required: true,
					assessment: { a: 1 }
				},
				{ status: 401 }
			)
		);
		const e = await api.get('/parse').catch((x) => x);
		expect(e).toMatchObject({ setupRequired: true, assessment: { a: 1 } });
	});

	it('keeps a settings refusal’s section', async () => {
		fetchMock.mockImplementation(async () =>
			json(
				{
					error: {
						code: 'invalid_setting',
						field: 'timeout_secs',
						message: 'Must be from 1 to 120.',
						section: 'crawler'
					}
				},
				{ status: 422 }
			)
		);
		const e = await api.patch('/settings', { body: {} }).catch((x) => x);
		expect(e).toMatchObject({
			status: 422,
			code: 'invalid_setting',
			field: 'timeout_secs',
			section: 'crawler'
		});
		expect(e.current).toBeUndefined();
	});

	it('keeps a stale revision’s current values', async () => {
		const crawler = {
			fields: {
				intensity: {
					env: null,
					read_only: false,
					restart_required: false,
					source: 'db',
					value: 'low'
				}
			},
			restart_required: [],
			revision: 5,
			section: 'crawler'
		};
		fetchMock.mockImplementation(async () =>
			json(
				{ error: { code: 'stale_revision', message: 'x' }, current: { crawler } },
				{ status: 409 }
			)
		);
		const e = await api.patch('/settings', { body: {} }).catch((x) => x);
		expect(e).toMatchObject({ status: 409, code: 'stale_revision', current: { crawler } });
		expect(e.section).toBeUndefined();
	});

	it('parses Retry-After as seconds or HTTP date', () => {
		const now = Date.parse('2026-10-22T10:00:00Z');
		expect(parseRetryAfter('120', now)).toBe(120);
		expect(parseRetryAfter('Thu, 22 Oct 2026 10:00:30 GMT', now)).toBe(30);
		expect(parseRetryAfter('Thu, 22 Oct 2026 09:00:00 GMT', now)).toBe(0);
		expect(parseRetryAfter('soon', now)).toBeUndefined();
		expect(parseRetryAfter(null, now)).toBeUndefined();
	});

	it('flags cursor errors for restart', () => {
		expect(mustRestartPaging(new ApiError(410, 'cursor_expired', ''))).toBe(true);
		expect(mustRestartPaging(new ApiError(400, 'bad_cursor', ''))).toBe(true);
		expect(mustRestartPaging(new ApiError(400, 'unknown_sort', ''))).toBe(false);
		expect(mustRestartPaging(new Error('x'))).toBe(false);
	});
});

describe('abort while reading the body', () => {
	const abortingResponse = (status: number) => {
		const res = new Response('{}', { status });
		res.json = () => Promise.reject(new DOMException('Aborted', 'AbortError'));
		return res;
	};

	it('stays an AbortError for an error body', async () => {
		fetchMock.mockImplementation(async () => abortingResponse(500));
		expect(isAbort(await api.get('/parse').catch((x) => x))).toBe(true);
	});

	it('stays an AbortError for a 200 body', async () => {
		const ctl = new AbortController();
		fetchMock.mockImplementation(async () => {
			ctl.abort();
			return abortingResponse(200);
		});
		expect(isAbort(await api.get('/parse', { signal: ctl.signal }).catch((x) => x))).toBe(true);
	});
});

describe('401 handling', () => {
	const unauth = () => envelope('login_required', 401);

	it('calls the handler once for concurrent 401s, and again after a success', async () => {
		const handler = vi.fn();
		onLoginRequired(handler);
		fetchMock.mockImplementation(async () => unauth());
		const rs = await Promise.allSettled([
			api.get('/parse'),
			api.get('/settings'),
			api.get('/filters')
		]);
		expect(rs.every((r) => r.status === 'rejected')).toBe(true);
		expect(handler).toHaveBeenCalledTimes(1);
		expect(handler).toHaveBeenCalledWith(location.pathname + location.search, {
			setupRequired: false
		});

		fetchMock.mockImplementation(async () => json({}));
		await api.get('/settings');
		fetchMock.mockImplementation(async () => unauth());
		await api.get('/parse').catch(() => {});
		expect(handler).toHaveBeenCalledTimes(2);
	});

	it('tells the handler when setup is required', async () => {
		const handler = vi.fn();
		onLoginRequired(handler);
		fetchMock.mockImplementation(async () =>
			json(
				{ error: { code: 'login_required', message: 'x' }, setup_required: true },
				{ status: 401 }
			)
		);
		await api.get('/parse').catch(() => {});
		expect(handler).toHaveBeenCalledWith(expect.any(String), { setupRequired: true });
	});

	it('a successful public /auth does not reset the de-duplication', async () => {
		const handler = vi.fn();
		onLoginRequired(handler);
		fetchMock.mockImplementation(async () => unauth());
		await api.get('/parse').catch(() => {});
		fetchMock.mockImplementation(async () => json({}));
		await api.get('/auth');
		fetchMock.mockImplementation(async () => unauth());
		await api.get('/parse').catch(() => {});
		expect(handler).toHaveBeenCalledTimes(1);
	});

	it('does not redirect for the auth endpoints themselves', async () => {
		const handler = vi.fn();
		onLoginRequired(handler);
		fetchMock.mockImplementation(async () => unauth());
		await api.post('/login', { body: { password: 'b' } }).catch(() => {});
		await api.get('/auth').catch(() => {});
		expect(handler).not.toHaveBeenCalled();
	});

	it('does not redirect while on /login', async () => {
		const handler = vi.fn();
		onLoginRequired(handler);
		history.replaceState(null, '', '/login?next=%2F');
		fetchMock.mockImplementation(async () => unauth());
		await api.get('/parse').catch(() => {});
		history.replaceState(null, '', '/');
		expect(handler).not.toHaveBeenCalled();
	});

	it('ignores other 401 codes', async () => {
		const handler = vi.fn();
		onLoginRequired(handler);
		fetchMock.mockImplementation(async () => envelope('invalid_credentials', 401));
		await api.get('/parse').catch(() => {});
		expect(handler).not.toHaveBeenCalled();
	});
});

describe('latest()', () => {
	const deferred = <T>() => {
		let resolve!: (v: T) => void;
		const promise = new Promise<T>((r) => (resolve = r));
		return { promise, resolve };
	};

	it('aborts the previous run when a new one starts', async () => {
		const slot = latest();
		const signals: AbortSignal[] = [];
		const d = deferred<string>();
		const first = slot.run((s) => (signals.push(s), d.promise)).catch((e) => e);
		slot.run(async (s) => (signals.push(s), 'b'));
		expect(signals[0]!.aborted).toBe(true);
		expect(signals[1]!.aborted).toBe(false);
		d.resolve('a');
		expect(isAbort(await first)).toBe(true);
	});

	it('drops an older response that resolves after a newer one, even if abort raced', async () => {
		const slot = latest();
		const a = deferred<string>();
		const b = deferred<string>();
		const seen: string[] = [];
		// Neither run honours its signal, so only the sequence check can save us.
		const pa = slot
			.run(() => a.promise)
			.then(
				(v) => seen.push(v),
				() => seen.push('dropped-a')
			);
		const pb = slot
			.run(() => b.promise)
			.then(
				(v) => seen.push(v),
				() => seen.push('dropped-b')
			);
		b.resolve('b');
		await pb;
		a.resolve('a');
		await pa;
		expect(seen).toEqual(['b', 'dropped-a']);
	});

	it('cancel() drops the in-flight run', async () => {
		const slot = latest();
		const d = deferred<string>();
		const p = slot.run(() => d.promise).catch((e) => e);
		slot.cancel();
		d.resolve('x');
		expect(isAbort(await p)).toBe(true);
	});

	it('passes real errors of the newest run through', async () => {
		const slot = latest();
		await expect(slot.run(() => Promise.reject(new ApiError(500, 'x', '')))).rejects.toMatchObject({
			code: 'x'
		});
	});
});
