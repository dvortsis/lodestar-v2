// A live `text/event-stream` for shell specs. Playwright can only fulfil a response in one piece,
// which ends the stream, so the page's `fetch` for `/api/v1/events` is replaced (before any page
// script runs) by one that answers with a stream the test controls: `send` writes a frame, `drop`
// ends the connection cleanly. Each new connection (a reconnect) gets the initial snapshot again.
import { expect, type Page } from '@playwright/test';
import type { components } from '../../src/lib/api/schema';

export type Crawler = components['schemas']['CrawlerEvent'];

export const crawlerEvent = (over: Partial<Crawler> = {}): Crawler => ({
	at: 1_700_000_000,
	dht_nodes: 480,
	fetch_active: 6,
	fetch_pending: 40,
	fetched: 120,
	harvested: 300,
	paused: false,
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

/** One synthetic interaction: an event the shell treats like the first key press. */
async function nudge(page: Page) {
	// A nudge that lands as the page navigates away (a 401 hand-off) has nothing left to do.
	await page.evaluate(() => window.dispatchEvent(new Event('focusin'))).catch(() => undefined);
}

const STREAM_PATH = '/api/v1/events';

/**
 * The shell starts its stream on the first interaction (or 30 s after load). Tests that need it
 * right away call this: it nudges every 100 ms until the stream has been asked for (a request for
 * `/api/v1/events`, routed or not, or one `liveEvents` answered), with a 15 s deadline. The chrome
 * that listens for the nudge mounts a moment after load, later on a slow runner, so a nudge count
 * or a fixed time can run out first. Call it before the stream starts: it waits for a new request.
 */
export async function engage(page: Page) {
	let asked = false;
	const seen = (request: { url(): string }) => {
		if (new URL(request.url()).pathname === STREAM_PATH) asked = true;
	};
	page.on('request', seen);
	try {
		await expect
			.poll(
				async () => {
					if (!asked) {
						const live = await page
							.evaluate(() => (window.__sse?.requests.length ?? 0) > 0)
							.catch(() => false);
						asked ||= live;
					}
					if (!asked) await nudge(page);
					return asked;
				},
				{ message: 'the shell never asked for its event stream', timeout: 15_000, intervals: [100] }
			)
			.toBe(true);
	} finally {
		page.off('request', seen);
	}
}

/**
 * Nudges every 100 ms for `ms`, for a test that checks nothing starts on an interaction (the
 * sign-in screens open no stream). That is an absence, with no condition to wait for, so the
 * window is a fixed time; nothing waits on it to pass.
 */
export async function nudgeFor(page: Page, ms: number) {
	const end = Date.now() + ms;
	await expect
		.poll(
			async () => {
				await nudge(page);
				return Date.now() >= end;
			},
			{ timeout: ms + 10_000, intervals: [100] }
		)
		.toBe(true);
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
			dispatchEvent(new Event('focusin'));
			if (++tries > 60) clearInterval(nudge);
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

/**
 * Opens the command palette with a key press, retrying until the shell's shortcuts exist (they
 * register a moment after the chrome mounts). Never presses again once the palette is open.
 */
export async function openPaletteWithKey(page: Page, press: () => Promise<void>) {
	const dialog = page.getByRole('dialog', { name: 'Command palette' });
	await expect(async () => {
		if (!(await dialog.isVisible())) await press();
		await expect(dialog).toBeVisible({ timeout: 800 });
	}).toPass({ timeout: 10_000 });
}
