// A scripted `fetch` for the flows' tests: answers by "METHOD /path" (API base stripped) and keeps
// every request's serialized body, so tests can assert on what went over the wire.
import { vi } from 'vitest';

export interface Sent {
	method: string;
	path: string;
	url: string;
	body: string | undefined;
}

type Reply = { status?: number; body?: unknown; headers?: Record<string, string> };
type Handler = Reply | ((req: Sent) => Reply | Promise<Reply>);

export function mockFetch() {
	const routes = new Map<string, Handler[]>();
	const sent: Sent[] = [];
	const fn = vi.fn(async (input: RequestInfo | URL, init: RequestInit = {}) => {
		const url = String(input);
		const path = url.replace(/^\/api\/v1/, '').split('?')[0] ?? '';
		const method = (init.method ?? 'GET').toUpperCase();
		const req: Sent = {
			method,
			path,
			url,
			body: init.body === undefined ? undefined : String(init.body)
		};
		sent.push(req);
		if (init.signal?.aborted) throw new DOMException('Aborted', 'AbortError');
		const key = `${method} ${path}`;
		const queue =
			routes.get(key) ?? routes.get(`${method} ${path.replace(/\/\d+(?=\/|$)/g, '/{id}')}`);
		const handler = queue && (queue.length > 1 ? queue.shift()! : queue[0]);
		if (!handler)
			return new Response(JSON.stringify({ error: { code: 'unmocked', message: key } }), {
				status: 501
			});
		const reply = typeof handler === 'function' ? await handler(req) : handler;
		return new Response(reply.body === undefined ? null : JSON.stringify(reply.body), {
			status: reply.status ?? 200,
			headers: { 'Content-Type': 'application/json', ...reply.headers }
		});
	});
	vi.stubGlobal('fetch', fn);
	return {
		sent,
		/** Queue answers for `METHOD /path` (numeric segments may be written `{id}`); the last repeats. */
		on(key: string, ...handlers: Handler[]) {
			routes.set(key, handlers);
			return this;
		},
		error(
			key: string,
			status: number,
			code: string,
			extra: Record<string, unknown> = {},
			headers?: Record<string, string>
		) {
			const { assessment, ...rest } = extra;
			return this.on(key, {
				status,
				headers,
				body: {
					error: { code, message: `server: ${code}`, ...rest },
					...(assessment ? { assessment } : {})
				}
			});
		},
		to(path: string) {
			return sent.filter((s) => s.path.startsWith(path));
		}
	};
}
