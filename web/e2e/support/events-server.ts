// A live `text/event-stream` for shell specs. Playwright can only fulfil a response in one piece,
// which ends the stream, so the page's `fetch` for `/api/v1/events` is replaced (before any page
// script runs) by one that answers with a stream the test controls: `send` writes a frame, `drop`
// ends the connection cleanly. Each new connection (a reconnect) gets the initial snapshot again.
import type { Page } from '@playwright/test';
import type { components } from '../../src/lib/api/schema';

export type Crawler = components['schemas']['CrawlerEvent'];

export const crawlerEvent = (over: Partial<Crawler> = {}): Crawler => ({
	at: 1_700_000_000,
	dht_nodes: 480,
	fetch_active: 6,
	fetch_pending: 40,
	fetched: 120,
	harvested: 300,
	stored: 90,
	torrents: 1_234_567,
	...over
});

declare global {
	interface Window {
		__sse?: {
			controllers: ReadableStreamDefaultController<Uint8Array>[];
			requests: string[];
			initial: unknown;
			id: number;
		};
	}
}

/**
 * The shell starts its stream on the first interaction (or 30 s after load). Tests that need it
 * right away send one: a synthetic event the shell treats like the first key press.
 */
export async function engage(page: Page) {
	// The chrome (and its listeners) mounts a moment after load; its skip link is the sign.
	await page.waitForSelector('a[href="#main"]', { state: 'attached' });
	await page.evaluate(() => window.dispatchEvent(new Event('focusin')));
}

export interface EventsHandle {
	/** Sends one frame on every open connection. */
	send(event: string, data: unknown): Promise<void>;
	/** Ends every open connection (a clean close). */
	drop(): Promise<void>;
	/** URLs the current document asked for. */
	requests(): Promise<string[]>;
}

export async function liveEvents(
	page: Page,
	initial: Crawler | null = crawlerEvent()
): Promise<EventsHandle> {
	await page.addInitScript((first) => {
		const state: NonNullable<Window['__sse']> = (window.__sse = {
			controllers: [],
			requests: [],
			initial: first,
			id: 0
		});
		const enc = new TextEncoder();
		const frame = (event: string, data: unknown) =>
			`id: 1-${++state.id}\nevent: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
		(window as unknown as { __frame: typeof frame }).__frame = frame;
		const real = window.fetch.bind(window);
		window.fetch = (input, init) => {
			const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
			if (!new URL(url, location.href).pathname.endsWith('/api/v1/events'))
				return real(input, init);
			state.requests.push(url);
			const body = new ReadableStream<Uint8Array>({
				start(controller) {
					state.controllers.push(controller);
					controller.enqueue(enc.encode('retry: 1000\n\n'));
					if (state.initial) controller.enqueue(enc.encode(frame('crawler', state.initial)));
					init?.signal?.addEventListener('abort', () => {
						try {
							controller.close();
						} catch {
							/* already closed */
						}
					});
				}
			});
			return Promise.resolve(
				new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } })
			);
		};
		// Keep nudging until the chrome has mounted and taken it.
		let tries = 0;
		const nudge = setInterval(() => {
			if (document.querySelector('a[href="#main"]')) {
				dispatchEvent(new Event('focusin'));
				clearInterval(nudge);
			} else if (++tries > 100) clearInterval(nudge);
		}, 100);
	}, initial);
	return {
		send: (event, data) =>
			page.evaluate(
				([e, d]) => {
					const enc = new TextEncoder();
					const f = (window as unknown as { __frame: (e: string, d: unknown) => string }).__frame;
					for (const c of window.__sse!.controllers) {
						try {
							c.enqueue(enc.encode(f(e as string, d)));
						} catch {
							/* closed */
						}
					}
				},
				[event, data] as const
			),
		drop: () =>
			page.evaluate(() => {
				for (const c of window.__sse!.controllers.splice(0)) {
					try {
						c.close();
					} catch {
						/* closed */
					}
				}
			}),
		requests: () => page.evaluate(() => window.__sse?.requests ?? [])
	};
}
