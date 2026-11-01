// "A newer Lodestar is running": the open tab's UI build against the server's. The server serves the
// bundle's `_app/version.json` (the same id as `/system` `ui_build_id`, ~40 bytes, revalidated), so
// one small GET on `visibilitychange` is the whole check. A failed or odd answer says nothing.

// Not `version` from `$app/environment`: it renames the shared runtime chunk's exports (see
// features/settings/about/logic.ts). SvelteKit defines this constant with the same value.
declare const __SVELTEKIT_APP_VERSION__: string;

export const STALE_THROTTLE_MS = 5 * 60_000;

export interface StaleDeps {
	/** The id of the build the server serves; null when it can't be read. */
	fetchBuild?: () => Promise<string | null>;
	now?: () => number;
	/** This tab's UI build. */
	build?: string;
}

export async function fetchBuild(): Promise<string | null> {
	try {
		const res = await fetch('/_app/version.json', { cache: 'no-store' });
		if (!res.ok) return null;
		const body: unknown = await res.json();
		const v = (body as { version?: unknown } | null)?.version;
		return typeof v === 'string' && v ? v : null;
	} catch {
		return null;
	}
}

/**
 * Checks when the tab is shown again, at most once per STALE_THROTTLE_MS and never two at once.
 * Calls `onStale` once and stops when the builds differ. Returns the stop.
 */
export function watchStale(onStale: () => void, deps: StaleDeps = {}): () => void {
	const get = deps.fetchBuild ?? fetchBuild;
	const now = deps.now ?? Date.now;
	const mine = deps.build ?? __SVELTEKIT_APP_VERSION__;
	let last = now();
	let busy = false;
	const stop = () => document.removeEventListener('visibilitychange', check);
	function check() {
		if (document.visibilityState !== 'visible' || busy || now() - last < STALE_THROTTLE_MS) return;
		busy = true;
		last = now();
		void get()
			.catch(() => null)
			.then((server) => {
				busy = false;
				if (server && server !== mine) {
					stop();
					onStale();
				}
			});
	}
	document.addEventListener('visibilitychange', check);
	return stop;
}

/**
 * Whether leaving now would be stopped by a leave guard (a Settings draft with unsaved changes).
 * Asks the guards the way the browser does: kit runs every `beforeNavigate` callback on
 * `beforeunload` and cancels it when one objects.
 */
export function hasUnsaved(): boolean {
	// Kit's handler also saves scroll and snapshot state, and (when nothing objects) hands scroll
	// restoration back to the browser; the latter is undone here.
	const restoration = history.scrollRestoration;
	const e = new Event('beforeunload', { cancelable: true });
	window.dispatchEvent(e);
	if (history.scrollRestoration !== restoration) history.scrollRestoration = restoration;
	return e.defaultPrevented;
}
