// OpenAPI-example mock for `/api/v1/**` (build-plan §1.3 seam 2), until the fixture server exists.
// Each request is matched to an operation in openapi.json (method + path template) and answered
// with that operation's documented example, or with a typed override from the test. A request
// that matches no operation, or an operation with neither example nor override, answers with a
// 501 error envelope AND fails the test at teardown, naming the operation. No silent 404s.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import type { Page, Request, Route } from '@playwright/test';
import type { operations } from '../../src/lib/api/schema';

type Json = unknown;
type Schema = Record<string, unknown>;
export interface Spec {
	servers?: { url: string }[];
	paths: Record<string, Record<string, OperationSpec>>;
	components: { schemas: Record<string, Schema>; responses?: Record<string, Schema> };
}
export interface OperationSpec {
	operationId?: string;
	responses: Record<string, Schema>;
}

export type OperationId = keyof operations;

/** The JSON body of an operation's first 2xx response, from the generated types. */
export type ResponseBody<K extends OperationId> = operations[K]['responses'] extends infer R
	? R[Extract<keyof R, 200 | 201 | 202>] extends { content: { 'application/json': infer B } }
		? B
		: unknown
	: never;

/** The body, or a full control over status/headers, or a function of the request. */
export type Override<K extends OperationId> =
	| ResponseBody<K>
	| { status: number; body?: Json; headers?: Record<string, string> }
	// A handler may answer later (a promise): `handle` awaits it, so a spec can hold a response.
	| ((
			request: Request
	  ) =>
			| ResponseBody<K>
			| { status: number; body?: Json }
			| Promise<ResponseBody<K> | { status: number; body?: Json }>);

const METHODS = ['get', 'post', 'put', 'patch', 'delete'];
const specUrl = new URL('../../src/lib/api/openapi.json', import.meta.url);

export interface Operation {
	id: string;
	method: string;
	template: string;
	regex: RegExp;
	example: { status: number; body: Json } | null;
}

export function loadOperations(
	spec: Spec = JSON.parse(readFileSync(fileURLToPath(specUrl), 'utf8'))
) {
	const resolve = (node: Schema | undefined): Schema | undefined => {
		let cur = node;
		for (let i = 0; cur && typeof cur.$ref === 'string' && i < 10; i++) {
			const name = (cur.$ref as string).split('/').pop()!;
			cur = cur.$ref.includes('/responses/')
				? spec.components.responses?.[name]
				: spec.components.schemas[name];
		}
		return cur;
	};
	const exampleOf = (responses: Record<string, Schema>) => {
		for (const [code, raw] of Object.entries(responses)) {
			if (!/^2\d\d$/.test(code)) continue;
			const json = (resolve(raw)?.content as Record<string, Schema> | undefined)?.[
				'application/json'
			];
			if (!json) continue;
			if ('example' in json) return { status: Number(code), body: json.example };
			const named = json.examples as Record<string, { value: Json }> | undefined;
			const first = named && Object.values(named)[0];
			if (first) return { status: Number(code), body: first.value };
			const schema = resolve(json.schema as Schema);
			if (schema && 'example' in schema) return { status: Number(code), body: schema.example };
		}
		return null;
	};
	const base = (spec.servers?.[0]?.url ?? '/api/v1').replace(/\/$/, '');
	const ops: Operation[] = [];
	for (const [template, item] of Object.entries(spec.paths)) {
		for (const method of METHODS) {
			const op = item[method];
			if (!op) continue;
			const pattern = template
				.split('/')
				.map((seg) => (/^\{.+\}$/.test(seg) ? '[^/]+' : seg.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
				.join('/');
			ops.push({
				id: op.operationId ?? `${method.toUpperCase()} ${template}`,
				method: method.toUpperCase(),
				template: `${method.toUpperCase()} ${base}${template}`,
				regex: new RegExp(`^${base}${pattern}/?$`),
				example: exampleOf(op.responses)
			});
		}
	}
	// Static segments beat parameters: `/filters/queue` before `/filters/{id}`.
	const params = (o: Operation) => (o.template.match(/\{/g) ?? []).length;
	return ops.sort((a, b) => params(a) - params(b));
}

export class OpenApiMock {
	constructor(readonly operations: Operation[] = loadOperations()) {}

	private overrides = new Map<string, Override<OperationId>>();
	private seen: { op: string; request: Request }[] = [];
	private failures: string[] = [];

	/** Answer `operationId` with `body` (typed from schema.d.ts), a status envelope, or a function. */
	on<K extends OperationId>(operationId: K, override: Override<K>): this {
		if (!this.operations.some((o) => o.id === operationId)) {
			throw new Error(`mock.on: no operation "${operationId}" in openapi.json`);
		}
		this.overrides.set(operationId, override as Override<OperationId>);
		return this;
	}

	/** Answer `operationId` with the shared error envelope (`{ error: { code, message, … } }`). */
	error(
		operationId: OperationId,
		status: number,
		code: string,
		message = code,
		extra: Record<string, unknown> = {}
	): this {
		return this.on(operationId, { status, body: { error: { code, message, ...extra } } });
	}

	/** Requests answered for `operationId` so far. */
	calls(operationId: OperationId): Request[] {
		return this.seen.filter((s) => s.op === operationId).map((s) => s.request);
	}

	/** Throws if any request went unanswered; the fixture calls this at teardown. */
	assertNoFailures() {
		if (this.failures.length) {
			throw new Error(
				`OpenAPI mock could not answer:\n  - ${[...new Set(this.failures)].join('\n  - ')}`
			);
		}
	}

	async install(page: Page) {
		await page.route('**/api/v1/**', (route) => this.handle(route));
	}

	private envelope(route: Route, status: number, code: string, message: string) {
		return route.fulfill({
			status,
			contentType: 'application/json',
			body: JSON.stringify({ error: { code, message } })
		});
	}

	private async handle(route: Route) {
		const request = route.request();
		const url = new URL(request.url());
		const op = this.operations.find(
			(o) => o.method === request.method() && o.regex.test(url.pathname)
		);
		if (!op) {
			const what = `${request.method()} ${url.pathname} matches no operation in openapi.json`;
			this.failures.push(what);
			return this.envelope(route, 501, 'mock_unmatched', what);
		}
		this.seen.push({ op: op.id, request });
		const override = this.overrides.get(op.id);
		// The app shell opens the event stream on every page. A live stream is a response that stays
		// open, so by default it is left pending (the status pill reads "Connecting"); a test that
		// needs events overrides `getEvents` (see e2e/support/events-server.ts, used by shell.routes.test.ts).
		if (op.id === 'getEvents' && override === undefined) return new Promise<void>(() => {});
		// The shell probes `GET /auth` when the stream cannot connect, to tell a blocked stream from a
		// server that is down. Any answer will do; tests that care override it.
		if (op.id === 'getAuthStatus' && override === undefined) {
			return route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({ logged_in: false, setup_required: false })
			});
		}
		// The search page's results panel (S-search-2) lists saved searches in its empty state once
		// anything engages the page, which most specs that pass through /search do. None by default.
		if (op.id === 'listSavedSearches' && override === undefined) {
			return route.fulfill({ status: 200, contentType: 'application/json', body: '[]' });
		}
		// The facet rail (S-search-2b) asks for counts beside the results at x-large: no groups by
		// default, so specs that don't test the rail needn't mock it.
		if (op.id === 'facetsPost' && override === undefined) {
			const empty = {
				backfill_before: null,
				buckets: { seeders: null, trackers: null },
				canonical: '',
				facets: {},
				histograms: {},
				sample_rate: null,
				sampled: false,
				took_ms: 0,
				total: 0,
				total_approx: false,
				unavailable: []
			};
			return route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify(empty)
			});
		}
		let status: number, body: Json, headers: Record<string, string> | undefined;
		const pick = (v: unknown) => {
			if (
				v &&
				typeof v === 'object' &&
				'status' in v &&
				typeof (v as { status: unknown }).status === 'number'
			) {
				const s = v as { status: number; body?: Json; headers?: Record<string, string> };
				return { status: s.status, body: s.body, headers: s.headers };
			}
			return { status: op.example?.status ?? 200, body: v, headers: undefined };
		};
		if (override !== undefined) {
			({ status, body, headers } = pick(
				typeof override === 'function' ? await override(request) : override
			));
		} else if (op.example) {
			({ status, body } = op.example);
		} else {
			const what = `operation "${op.id}" (${op.template}) has no documented example and no mock.on() override`;
			this.failures.push(what);
			return this.envelope(route, 501, 'mock_no_example', what);
		}
		await route.fulfill({
			status,
			headers,
			contentType: 'application/json',
			body: body === undefined ? '' : JSON.stringify(body)
		});
	}
}
