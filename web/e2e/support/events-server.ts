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
	searchable: 1_234_567,
	stored: 90,
	torrents: 1_240_000,
	...over
});

declare global {
	interface Window {
		__sse?: {
			controllers: ReadableStreamDefaultController<Uint8Array>[];
			/** Per connection (same order as `controllers`): its `?topics=` and whether it is open. */
			streams: { topics: string[]; open: boolean }[];
			/** Per pathname: how many JSON bodies the page has finished reading. */
			answered: Record<string, number>;
			requests: string[];
			initial: unknown;
			id: number;
		};
	}
}

/** One synthetic interaction: an event the shell treats like the first key press. */
async function nudge(page: Page) {
	// One that lands as the page navigates away (a 401 hand-off) has nothing left to do.
	await page.evaluate(() => window.dispatchEvent(new Event('focusin'))).catch(() => undefined);
}

/**
 * The shell starts its chrome and stream on the first interaction (or 30 s after load). Tests that
 * need them right away call this: it waits for `<main id="main">`, which only `Shell.svelte`
 * renders, in the same mount that attaches the shell's listener. (`#boot` is no signal: on the
 * sign-in screens the inline boot script removes it before the app starts.) So the one nudge is
 * always taken, however late the shell mounts. The caller's own assertion is the condition that the
 * chrome or the stream got there.
 */
export async function engage(page: Page) {
	await expect(page.locator('main#main')).toBeAttached();
	await nudge(page);
}

/**
 * Waits until the page's network has been quiet (nothing in flight) for `quietMs`, with a 15 s
 * deadline. Counts from now: start it before the action whose requests it should cover.
 */
export function networkQuiet(page: Page, quietMs = 500) {
	let inFlight = 0;
	let last = Date.now();
	const start = () => {
		inFlight++;
		last = Date.now();
	};
	const end = () => {
		inFlight = Math.max(0, inFlight - 1);
		last = Date.now();
	};
	page.on('request', start);
	page.on('requestfinished', end);
	page.on('requestfailed', end);
	return async () => {
		try {
			await expect
				.poll(() => inFlight === 0 && Date.now() - last >= quietMs, {
					message: 'the page kept a request open',
					timeout: 15_000,
					intervals: [100]
				})
				.toBe(true);
		} finally {
			page.off('request', start);
			page.off('requestfinished', end);
			page.off('requestfailed', end);
		}
	};
}

/**
 * For a screen that must open no stream (sign-in, setup, welcome): engages, waits until the chrome
 * has mounted and taken the interaction (its toaster region, which the bare chrome has too), then
 * until the network has gone quiet, so every chunk the interaction loads has arrived. A stream the
 * shell started by mistake shows in `liveEvents`' requests, or keeps the network busy and fails here.
 */
export async function engageBare(page: Page) {
	const quiet = networkQuiet(page);
	await engage(page);
	await expect(page.getByRole('region', { name: 'Notifications' })).toBeAttached();
	await quiet();
}

export interface EventsHandle {
	/**
	 * Sends one frame on every open connection, once one is open whose `?topics=` has `event` (any
	 * open one for `resync`), with a 10 s deadline. A frame sent before then would be lost silently.
	 * Await it: an unawaited send that times out loses its message, and two can arrive out of order.
	 */
	send(event: string, data: unknown): Promise<void>;
	/** Ends every open connection (a clean close). */
	drop(): Promise<void>;
	/** URLs the current document asked for. */
	requests(): Promise<string[]>;
	/**
	 * How many JSON bodies from `pathname` (e.g. `/api/v1/search/live`) the page has read, error
	 * bodies included (a 429 counts too). Once it counts a response, the code that awaited that body
	 * has already run.
	 */
	answered(pathname: string): Promise<number>;
}

export async function liveEvents(
	page: Page,
	initial: Crawler | null = crawlerEvent()
): Promise<EventsHandle> {
	await page.addInitScript((first) => {
		const state: NonNullable<Window['__sse']> = (window.__sse = {
			controllers: [],
			streams: [],
			answered: {},
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
			const parsed = new URL(url, location.href);
			if (!parsed.pathname.endsWith('/api/v1/events')) {
				// Counts each JSON body once the page has read it. The count goes up inside the promise
				// the caller awaits, so a later `page.evaluate` that sees it also sees what the caller
				// did with the body (`answered`).
				return real(input, init).then((res) => {
					const json = res.json.bind(res);
					res.json = () =>
						json().then((v: unknown) => {
							state.answered[parsed.pathname] = (state.answered[parsed.pathname] ?? 0) + 1;
							return v;
						});
					return res;
				});
			}
			state.requests.push(url);
			const stream = {
				topics: (parsed.searchParams.get('topics') ?? '').split(',').filter(Boolean),
				open: true
			};
			const body = new ReadableStream<Uint8Array>({
				start(controller) {
					state.controllers.push(controller);
					state.streams.push(stream);
					controller.enqueue(enc.encode('retry: 1000\n\n'));
					if (state.initial) controller.enqueue(enc.encode(frame('crawler', state.initial)));
					init?.signal?.addEventListener('abort', () => {
						stream.open = false;
						try {
							controller.close();
						} catch {
							/* already closed */
						}
					});
				},
				cancel() {
					stream.open = false;
				}
			});
			return Promise.resolve(
				new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } })
			);
		};
		// Keep nudging until the chrome has mounted and taken it: until the stream is asked for, or
		// for 30 s (the shell's own fallback), however late the page hydrates.
		let tries = 0;
		const nudge = setInterval(() => {
			if (state.requests.length || ++tries > 300) return clearInterval(nudge);
			dispatchEvent(new Event('focusin'));
		}, 100);
	}, initial);
	const listening = (event: string) =>
		page.evaluate(
			(e) =>
				!!window.__sse?.streams.some((s) => s.open && (e === 'resync' || s.topics.includes(e))),
			event
		);
	return {
		send: async (event, data) => {
			await expect
				.poll(() => listening(event), {
					message: `no open /events stream takes \`${event}\``,
					timeout: 10_000
				})
				.toBe(true);
			await page.evaluate(
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
			);
		},
		drop: () =>
			page.evaluate(() => {
				for (const s of window.__sse!.streams.splice(0)) s.open = false;
				for (const c of window.__sse!.controllers.splice(0)) {
					try {
						c.close();
					} catch {
						/* closed */
					}
				}
			}),
		requests: () => page.evaluate(() => window.__sse?.requests ?? []),
		answered: (pathname) => page.evaluate((p) => window.__sse?.answered[p] ?? 0, pathname)
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
