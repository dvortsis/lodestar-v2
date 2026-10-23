// Restarting Lodestar and finding it again (§10.10.6). `POST /system/restart` answers first; the
// server then goes down and comes back, so the client polls: `/healthz` with a growing wait
// (1 s doubling to 8 s) for up to 2 minutes. A same-origin answer isn't proof by itself, since the
// old process can still answer until it shuts down: `waitForRestart` also compares `started_at`.
import { api } from '$lib/api/client';
import { healthUrl } from './urls';

export const BACKOFF_START_MS = 1_000;
export const BACKOFF_CAP_MS = 8_000;
export const GIVE_UP_MS = 120_000;

/** 1 s, 2 s, 4 s, 8 s, 8 s, … */
export const backoff = (attempt: number): number =>
	Math.min(BACKOFF_CAP_MS, BACKOFF_START_MS * 2 ** attempt);

export interface Wait {
	/** Called between attempts, to learn how long it has been. Defaults to the clock. */
	now?: () => number;
	/** Resolves after `ms`; replaced by tests. */
	sleep?: (ms: number, signal?: AbortSignal) => Promise<void>;
	signal?: AbortSignal;
}

function defaultSleep(ms: number, signal?: AbortSignal): Promise<void> {
	return new Promise((resolve) => {
		const timer = setTimeout(resolve, ms);
		signal?.addEventListener(
			'abort',
			() => {
				clearTimeout(timer);
				resolve();
			},
			{ once: true }
		);
	});
}

export type Outcome = 'up' | 'timeout' | 'aborted';

/** Polls `attempt()` (true when the server is back) with the backoff, until the deadline. */
export async function pollUntil(attempt: () => Promise<boolean>, w: Wait = {}): Promise<Outcome> {
	const now = w.now ?? Date.now;
	const sleep = w.sleep ?? defaultSleep;
	const start = now();
	for (let n = 0; ; n++) {
		if (w.signal?.aborted) return 'aborted';
		if (await attempt().catch(() => false)) return 'up';
		if (w.signal?.aborted) return 'aborted';
		if (now() - start >= GIVE_UP_MS) return 'timeout';
		await sleep(backoff(n), w.signal);
	}
}

/**
 * This server is back after a restart: `/healthz` answers and `/system` shows a newer start
 * (`started_at`). When `/system` can't be read (the session is gone) the answer alone counts.
 */
export function restartedHere(before: number, fetcher: typeof fetch = fetch) {
	return async (): Promise<boolean> => {
		const res = await fetcher('/healthz', { cache: 'no-store' });
		if (!res.ok) return false;
		try {
			const system = await api.get('/system');
			return system.started_at !== before;
		} catch (e) {
			// Still starting up (`network`, 5xx): not yet. Anything else (a 401): it's up.
			const status = (e as { status?: number }).status ?? 0;
			return status !== 0 && status < 500;
		}
	};
}

/**
 * Another address is up. A cross-origin `fetch` is opaque with `no-cors`: any answer at all means
 * a server is listening there, which is all a move to a new address needs.
 */
export function upAt(address: string, fetcher: typeof fetch = fetch) {
	return async (): Promise<boolean> => {
		const url = healthUrl(address);
		if (!url) return false;
		await fetcher(url, { mode: 'no-cors', cache: 'no-store' });
		return true;
	};
}
