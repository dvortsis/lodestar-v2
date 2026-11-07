// The fleet pill's data (§19.7): `GET /insights/workers/summary`, polled at most once a minute and
// only while the tab is visible. `null` is a single machine, a failed request, or no answer yet.
import { api } from '$lib/api/client';
import type { components } from '$lib/api/schema';

export type Fleet = components['schemas']['Fleet'];
export const FLEET_POLL_MS = 60_000;

/** Starts polling; calls `set` with the fleet or null. Returns the stop (aborts any request). */
export function watchFleet(set: (f: Fleet | null) => void, doc: Document = document): () => void {
	let ctl: AbortController | undefined;
	let timer: ReturnType<typeof setTimeout> | undefined;
	let last = Number.NEGATIVE_INFINITY;
	let stopped = false;

	const arm = () => {
		clearTimeout(timer);
		if (stopped || doc.hidden) return;
		timer = setTimeout(() => void tick(), Math.max(0, last + FLEET_POLL_MS - Date.now()));
	};
	async function tick() {
		if (stopped || doc.hidden) return;
		ctl?.abort();
		ctl = new AbortController();
		last = Date.now();
		try {
			const r = await api.get('/insights/workers/summary', { signal: ctl.signal });
			if (!stopped) set(r.fleet);
		} catch (e) {
			if ((e as Error)?.name === 'AbortError') return;
			if (!stopped) set(null);
		}
		arm();
	}
	const onVisibility = () => {
		if (doc.hidden) {
			clearTimeout(timer);
			ctl?.abort();
		} else if (Date.now() - last >= FLEET_POLL_MS) void tick();
		else arm();
	};
	doc.addEventListener('visibilitychange', onVisibility);
	void tick();
	return () => {
		stopped = true;
		clearTimeout(timer);
		ctl?.abort();
		doc.removeEventListener('visibilitychange', onVisibility);
	};
}
