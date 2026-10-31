// What the server says logging is doing now (`GET /logs/catalog`): the events, each data kind's
// deadline and the presets. The screen's banners, the event list and the data kinds read it, and
// every privacy change reloads it, so what shows is always the server's state (§12.1). The
// server's clock is kept as an offset, since deadlines are on it, not the browser's.
import { api, isAbort, type ApiError } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import type { Catalog } from './rules';

export type DeploymentKind = components['schemas']['Deployment']['kind'];

/** How often the time left is redrawn and the catalog reloaded. */
export const TICK_MS = 30_000;

export class LoggingCatalog {
	catalog = $state.raw<Catalog>();
	error = $state.raw<ApiError | Error>();
	/** The server's clock, in Unix seconds, as of the last tick. */
	now = $state(0);
	/** How Lodestar is installed (`GET /system`), for the Docker hint and the Windows Event Log
	 * setting; undefined until read, or if it can't be, so neither shows. */
	deployment = $state<DeploymentKind>();
	#offset = 0;
	#seq = 0;
	#busy = 0;
	#started = 0;
	#timer: ReturnType<typeof setInterval> | undefined;

	/** Loads the catalog; a reply older than a newer request is dropped. */
	async load(signal?: AbortSignal): Promise<Catalog | undefined> {
		const mine = ++this.#seq;
		this.#busy++;
		this.#started = Date.now();
		try {
			const c = await api.get('/logs/catalog', { signal });
			if (mine !== this.#seq) return this.catalog;
			this.#offset = c.now - Date.now() / 1000;
			this.catalog = c;
			this.error = undefined;
			this.now = c.now;
			return c;
		} catch (e) {
			if (mine === this.#seq && !isAbort(e)) this.error = e as Error;
			return undefined;
		} finally {
			this.#busy--;
		}
	}

	/** Reloads unless a load started within the last tick is still under way, so reloads never
	 * stack, yet a request that never settles can't stop them for good. A failed one keeps the last
	 * catalog (`load` sets only `error`). */
	refresh() {
		if (!this.#busy || Date.now() - this.#started >= TICK_MS - 1000) {
			void this.load(AbortSignal.timeout(TICK_MS));
		}
	}

	/** The server's clock now. */
	serverNow(): number {
		return Date.now() / 1000 + this.#offset;
	}

	/** Redraws the time left, and reloads the catalog, so a kind turned on elsewhere shows up. */
	tick() {
		this.now = this.serverNow();
		this.refresh();
	}

	start(): () => void {
		void this.load();
		api.get('/system').then(
			(s) => (this.deployment = s.deployment.kind),
			() => {}
		);
		this.#timer = setInterval(() => this.tick(), TICK_MS);
		// Back in this tab: what another tab or the server changed shows at once.
		const visible = () => {
			if (document.visibilityState === 'visible') this.refresh();
		};
		document.addEventListener('visibilitychange', visible);
		return () => {
			clearInterval(this.#timer);
			document.removeEventListener('visibilitychange', visible);
		};
	}
}
