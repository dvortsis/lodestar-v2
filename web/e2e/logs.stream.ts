// A controllable `GET /logs/stream` for the live-log specs. Playwright can only fulfil a response
// in one piece, which would end the stream, so the page's `fetch` for the stream is replaced by one
// that answers with a stream the test writes to (as events-server.ts does for `/events`).
import { expect, type Page } from '@playwright/test';
import type { components } from '../src/lib/api/schema';

export type LogLine = components['schemas']['LogLine'];

declare global {
	interface Window {
		__logs?: { controllers: ReadableStreamDefaultController<Uint8Array>[]; urls: string[] };
	}
}

export const logLine = (n: number, over: Partial<LogLine> = {}): LogLine => ({
	component: 'dht',
	event: null,
	fields: [],
	id: `0a-${n}`,
	level: 'info',
	msg: `message ${n}`,
	opt_in: null,
	repeated: null,
	target: null,
	truncated: false,
	ts: '2026-10-31T10:00:00.000Z',
	...over
});

export async function liveLogs(page: Page) {
	await page.addInitScript(() => {
		const state = (window.__logs = { controllers: [], urls: [] } as NonNullable<Window['__logs']>);
		const real = window.fetch.bind(window);
		window.fetch = (input, init) => {
			const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
			if (!new URL(url, location.href).pathname.endsWith('/api/v1/logs/stream'))
				return real(input, init);
			state.urls.push(url);
			const body = new ReadableStream<Uint8Array>({
				start(controller) {
					state.controllers.push(controller);
					controller.enqueue(new TextEncoder().encode('retry: 1000\n\n'));
					init?.signal?.addEventListener('abort', () => {
						try {
							controller.close();
						} catch {
							/* closed */
						}
					});
				}
			});
			return Promise.resolve(
				new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } })
			);
		};
	});
	return {
		send: (lines: LogLine[]) =>
			page.evaluate((ls) => {
				const enc = new TextEncoder();
				for (const l of ls)
					for (const c of window.__logs!.controllers) {
						try {
							c.enqueue(enc.encode(`id: ${l.id}\nevent: line\ndata: ${JSON.stringify(l)}\n\n`));
						} catch {
							/* closed */
						}
					}
			}, lines),
		urls: () => page.evaluate(() => window.__logs?.urls ?? []),
		opened: () =>
			expect
				.poll(() => page.evaluate(() => window.__logs?.controllers.length ?? 0), {
					timeout: 10_000
				})
				.toBeGreaterThan(0)
	};
}
