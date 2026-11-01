// Typed `fetch` wrapper over the generated `paths` (schema.d.ts). Same-origin cookies, JSON
// writes, the error envelope as `ApiError`, one de-duplicated 401 handler, latest-wins slots.
import type { components, paths } from './schema';

const BASE = '/api/v1';
/** Calls to these never trigger the login redirect (a failed login is a 401 too). */
const AUTH_PATHS = new Set(['/login', '/setup', '/auth', '/logout']);
const AUTH_PAGES = ['/login', '/setup'];

export type ErrorBody = components['schemas']['ErrorBody'];
type ErrorEnvelope = components['schemas']['ErrorEnvelope'];
type SettingsErrorEnvelope = components['schemas']['SettingsErrorEnvelope'];

export class ApiError extends Error {
	readonly status: number;
	readonly code: string;
	readonly field?: string;
	/** Byte span within `field` (query-syntax errors). */
	readonly start?: number;
	readonly end?: number;
	/** Seconds to wait, from `Retry-After` (429/503). */
	readonly retryAfter?: number;
	/** On a 401: the instance still needs its first-run setup (envelope top level). */
	readonly setupRequired?: boolean;
	/** On a 428 from `POST /filters/apply`: the warning to show first (envelope top level). */
	readonly assessment?: ErrorEnvelope['assessment'];
	/** A refused settings change: the section at fault. */
	readonly section?: string;
	/** The cause as a stable code, for a `code` with several (`install_unavailable`'s `busy`). */
	readonly reason?: string;
	/** On a 409 `stale_revision`: each stale section's values now (envelope top level). */
	readonly current?: SettingsErrorEnvelope['current'];
	constructor(
		status: number,
		code: string,
		message: string,
		extra: Pick<ErrorBody, 'field' | 'start' | 'end' | 'reason'> &
			Pick<ErrorEnvelope, 'setup_required' | 'assessment'> &
			Pick<SettingsErrorEnvelope, 'current'> & { section?: string; retryAfter?: number } = {}
	) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
		this.code = code;
		this.field = extra.field;
		this.start = extra.start;
		this.end = extra.end;
		this.retryAfter = extra.retryAfter;
		this.setupRequired = extra.setup_required;
		this.assessment = extra.assessment;
		this.section = extra.section;
		this.reason = extra.reason;
		this.current = extra.current;
	}
}

export function abortError(): DOMException {
	return new DOMException('Aborted', 'AbortError');
}

/** True for a cancelled or superseded request: callers ignore these. */
export function isAbort(e: unknown): boolean {
	return typeof e === 'object' && e !== null && (e as { name?: unknown }).name === 'AbortError';
}

/** `cursor_expired` / `bad_cursor`: drop the cursor and restart from page 1. */
export function mustRestartPaging(e: unknown): boolean {
	return e instanceof ApiError && (e.code === 'cursor_expired' || e.code === 'bad_cursor');
}

const HTTP_DATE = /^[A-Za-z]{3}, \d{2} [A-Za-z]{3} \d{4} \d{2}:\d{2}:\d{2} GMT$/;

/** `Retry-After` in seconds: delta-seconds or an HTTP-date. Undefined when absent or invalid. */
export function parseRetryAfter(value: string | null, now = Date.now()): number | undefined {
	if (value === null) return undefined;
	const v = value.trim();
	if (/^\d+$/.test(v)) return Number(v);
	if (!HTTP_DATE.test(v)) return undefined;
	const at = Date.parse(v);
	return Number.isNaN(at) ? undefined : Math.max(0, Math.ceil((at - now) / 1000));
}

/** Turns a non-2xx response into an `ApiError` (envelope, or `http_<status>` for other bodies). */
export async function errorFromResponse(res: Response): Promise<ApiError> {
	const retryAfter = parseRetryAfter(res.headers.get('Retry-After'));
	let body: Partial<Omit<ErrorEnvelope, 'error'>> &
		Pick<SettingsErrorEnvelope, 'current'> & { error?: Partial<ErrorBody> & { section?: string } } =
		{};
	try {
		body = (await res.json()) ?? {};
	} catch (x) {
		if (isAbort(x)) throw abortError();
		// not JSON: a proxy error page
	}
	const e = body.error;
	if (e && typeof e.code === 'string') {
		return new ApiError(res.status, e.code, String(e.message ?? ''), {
			field: e.field,
			start: e.start,
			end: e.end,
			reason: e.reason,
			retryAfter,
			setup_required: body.setup_required,
			assessment: body.assessment,
			section: e.section,
			current: body.current
		});
	}
	return new ApiError(res.status, `http_${res.status}`, res.statusText, { retryAfter });
}

type LoginHandler = (next: string, opts: { setupRequired: boolean }) => void;
let loginHandler: LoginHandler | undefined;
let loginPending = false;

/**
 * The shell registers where a 401 goes. `next` is the current path and query; `setupRequired`
 * says the instance still needs its first-run setup (go to /setup, not /login).
 */
export function onLoginRequired(handler: LoginHandler | undefined): void {
	loginHandler = handler;
	loginPending = false;
}

function loginRequired(path: string, setupRequired: boolean): void {
	const loc = globalThis.location;
	if (!loginHandler || loginPending || AUTH_PATHS.has(path)) return;
	if (loc && AUTH_PAGES.some((p) => loc.pathname === p || loc.pathname.startsWith(p + '/'))) return;
	loginPending = true;
	loginHandler(loc ? loc.pathname + loc.search : '/', { setupRequired });
}

/** Called for every 401 `login_required` (the events client uses it too). */
export function handleUnauthorized(e: unknown, path: string): void {
	if (e instanceof ApiError && e.status === 401 && e.code === 'login_required')
		loginRequired(path, e.setupRequired === true);
}

/** Any successful response proves the session works, so a later 401 may redirect again. */
export function sessionOk(path = ''): void {
	// A public GET /auth says nothing about the session; a login or setup does.
	if (path === '/auth' || path === '/logout') return;
	loginPending = false;
	for (const f of [...sessionListeners]) f();
}

const sessionListeners = new Set<() => void>();

/** Runs `f` after every request that shows the session works. Returns the unsubscribe. */
export function onSessionOk(f: () => void): () => void {
	sessionListeners.add(f);
	return () => void sessionListeners.delete(f);
}

type Method = 'get' | 'post' | 'put' | 'patch' | 'delete';
type PathsWith<M extends Method> = {
	[P in keyof paths]: paths[P][M] extends object ? P : never;
}[keyof paths];
type Op<P extends keyof paths, M extends Method> = paths[P][M];

type Params<O, K extends 'path' | 'query'> = O extends { parameters: infer P }
	? P extends { [k in K]?: infer T }
		? Exclude<T, undefined>
		: never
	: never;
/** `K` is required when the operation declares it required, optional when not, absent when never. */
type Part<O, K extends 'path' | 'query'> = [Params<O, K>] extends [never]
	? { [k in K]?: never }
	: O extends { parameters: Record<K, object> }
		? { [k in K]: Params<O, K> }
		: { [k in K]?: Params<O, K> };
type BodyPart<O, M extends Method> = M extends 'get'
	? { body?: never }
	: O extends { requestBody: { content: { 'application/json': infer B } } }
		? { body: B }
		: O extends { requestBody?: { content: { 'application/json': infer B } } }
			? { body?: B }
			: { body?: never };

export type RequestOptions<O, M extends Method> = Part<O, 'path'> &
	Part<O, 'query'> &
	BodyPart<O, M> & { signal?: AbortSignal };
type AnyRequired<T> = Record<string, never> extends T ? false : true;
type Args<O, M extends Method> =
	AnyRequired<RequestOptions<O, M>> extends true
		? [options: RequestOptions<O, M>]
		: [options?: RequestOptions<O, M>];

type JsonBody<R> = R extends { content: { 'application/json': infer B } } ? B : void;
type OkResponses<O> = O extends { responses: infer R }
	? R[Extract<keyof R, 200 | 201 | 202>]
	: never;
type OkOf<O> = [OkResponses<O>] extends [never] ? void : JsonBody<OkResponses<O>>;

function url(path: string, params?: object, query?: object): string {
	let p = path;
	for (const [k, v] of Object.entries(params ?? {})) {
		p = p.replace(`{${k}}`, encodeURIComponent(String(v)));
	}
	const qs = new URLSearchParams();
	for (const [k, v] of Object.entries(query ?? {})) {
		if (v !== undefined && v !== null) qs.set(k, String(v));
	}
	const s = qs.toString();
	return BASE + p + (s ? `?${s}` : '');
}

async function request(
	method: Method,
	path: string,
	o: { query?: object; path?: object; body?: unknown; signal?: AbortSignal }
): Promise<unknown> {
	const headers: Record<string, string> = { Accept: 'application/json' };
	// The server's same-origin check rejects writes that aren't JSON.
	if (method !== 'get') headers['Content-Type'] = 'application/json';
	let res: Response;
	try {
		res = await fetch(url(path, o.path, o.query), {
			method: method.toUpperCase(),
			headers,
			body: o.body === undefined ? undefined : JSON.stringify(o.body),
			credentials: 'same-origin',
			signal: o.signal
		});
	} catch (e) {
		if (isAbort(e) || o.signal?.aborted) throw abortError();
		throw new ApiError(0, 'network', e instanceof Error ? e.message : 'network error');
	}
	if (!res.ok) {
		const err = await errorFromResponse(res);
		handleUnauthorized(err, path);
		throw err;
	}
	sessionOk(path);
	if (res.status === 204) return undefined;
	try {
		return await res.json();
	} catch (e) {
		if (isAbort(e) || o.signal?.aborted) throw abortError();
		throw new ApiError(res.status, 'bad_response', 'Response was not JSON');
	}
}

const call =
	<M extends Method>(method: M) =>
	<P extends PathsWith<M>>(path: P, ...args: Args<Op<P, M>, M>): Promise<OkOf<Op<P, M>>> =>
		request(method, path as string, ((args as unknown[])[0] ?? {}) as object) as never;

export const api = {
	get: call('get'),
	post: call('post'),
	put: call('put'),
	patch: call('patch'),
	delete: call('delete')
};

/**
 * A request slot: starting a run aborts the previous one, and a run that resolves after a
 * newer one started is dropped (it rejects with an abort error, see `isAbort`), even if the
 * abort lost the race. §10.12: stale responses can't overwrite newer ones.
 */
export function latest() {
	let seq = 0;
	let ctl: AbortController | undefined;
	return {
		async run<T>(fn: (signal: AbortSignal) => Promise<T>): Promise<T> {
			const mine = ++seq;
			ctl?.abort();
			const c = (ctl = new AbortController());
			try {
				const v = await fn(c.signal);
				if (mine !== seq) throw abortError();
				return v;
			} catch (e) {
				if (mine !== seq) throw abortError();
				throw e;
			}
		},
		cancel() {
			seq++;
			ctl?.abort();
		}
	};
}
