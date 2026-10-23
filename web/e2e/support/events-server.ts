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

/**
 * The shell starts its stream on the first interaction (or 30 s after load). Tests that need it
 * right away send one: a synthetic event the shell treats like the first key press. Sent for four
 * seconds, since the chrome that listens for it mounts a moment after load.
 */
export async function engage(page: Page) {
	// The chrome's listeners attach when it mounts, a moment after load: nudge until it has.
	for (let i = 0; i < 40; i++) {
		await page.evaluate(() => window.dispatchEvent(new Event('focusin')));
		// Paces the nudges: whether the chrome has mounted is not observable on every screen (the
		// sign-in screens have none), so this keeps sending rather than waiting for a signal.
		await page.waitForTimeout(100);
	}
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
