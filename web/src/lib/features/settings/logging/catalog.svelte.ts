// What the server says logging is doing now (`GET /logs/catalog`): the events, each data kind's
// deadline and the presets. The screen's banners, the event list and the data kinds read it, and
// every privacy change reloads it, so what shows is always the server's state (§12.1). The
// server's clock is kept as an offset, since deadlines are on it, not the browser's.
import { api, isAbort, type ApiError } from '$lib/api/client';
import type { Catalog } from './rules';

/** How often the time left is redrawn, and the catalog reloaded once a deadline passes. */
export const TICK_MS = 30_000;

export class LoggingCatalog {
	catalog = $state.raw<Catalog>();
	error = $state.raw<ApiError | Error>();
	/** The server's clock, in Unix seconds, as of the last tick. */
	now = $state(0);
	#offset = 0;
	#seq = 0;
	#timer: ReturnType<typeof setInterval> | undefined;

	/** Loads the catalog; a reply older than a newer request is dropped. */
	async load(): Promise<Catalog | undefined> {
		const mine = ++this.#seq;
		try {
			const c = await api.get('/logs/catalog');
			if (mine !== this.#seq) return this.catalog;
			this.#offset = c.now - Date.now() / 1000;
			this.catalog = c;
			this.error = undefined;
			this.now = c.now;
			return c;
		} catch (e) {
			if (mine === this.#seq && !isAbort(e)) this.error = e as Error;
			return undefined;
		}
	}

	/** The server's clock now. */
	serverNow(): number {
		return Date.now() / 1000 + this.#offset;
	}

	/** Redraws the time left, and reloads once a deadline the catalog listed has passed. */
	tick() {
		this.now = this.serverNow();
		const c = this.catalog;
		if (c?.kinds.some((k) => k.until > 0 && k.until <= this.now)) void this.load();
	}

	start(): () => void {
		void this.load();
		this.#timer = setInterval(() => this.tick(), TICK_MS);
		return () => clearInterval(this.#timer);
	}
}
