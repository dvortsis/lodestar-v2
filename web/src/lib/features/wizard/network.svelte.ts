// The network check step's state (§10.10.10, §18.7): `GET /network/check` read until the server has
// an answer, a Retest through the 30 s-limited `POST`. One request at a time: the next read is
// scheduled when the last one has finished, and everything stops at `destroy()` or the deadline.
import { api, ApiError, isAbort } from '$lib/api/client';
import type { components } from '$lib/api/schema';

export type NetworkCheck = components['schemas']['NetworkCheck'];

export interface NetworkOptions {
	/** Between reads while the answer is unknown. */
	intervalMs?: number;
	/** After this long without an answer, reading stops and Retest is offered. */
	deadlineMs?: number;
}

export const POLL_MS = 5_000;
/** The server measures for 10 minutes (`window_s`); a little longer than that. */
export const DEADLINE_MS = 12 * 60_000;

export type RetestState = 'idle' | 'busy' | 'limited' | 'error';

export function createNetworkCheck(options: NetworkOptions = {}) {
	const interval = options.intervalMs ?? POLL_MS;
	const deadline = options.deadlineMs ?? DEADLINE_MS;
	let check = $state<NetworkCheck | null>(null);
	let failed = $state(false);
	let stopped = $state(false);
	let retest = $state<RetestState>('idle');
	let retryAfter = $state<number | undefined>();
	let timer: ReturnType<typeof setTimeout> | undefined;
	let controller: AbortController | undefined;
	let gen = 0;
	let gone = false;
	let startedAt = Date.now();

	function schedule() {
		clearTimeout(timer);
		timer = undefined;
		if (gone) return;
		// Only an unknown answer is worth reading again.
		if (check && check.reachable !== null) return;
		if (Date.now() - startedAt >= deadline) {
			stopped = true;
			return;
		}
		timer = setTimeout(() => void read(), interval);
	}

	/** Starts a request; a newer one (or `destroy`) makes this one's answer count for nothing. */
	function begin() {
		clearTimeout(timer);
		controller?.abort();
		controller = new AbortController();
		return { id: ++gen, signal: controller.signal };
	}
	const current = (id: number) => !gone && id === gen;

	async function read() {
		const { id, signal } = begin();
		try {
			const next = await api.get('/network/check', { signal });
			if (!current(id)) return;
			check = next;
			failed = false;
		} catch (e) {
			if (isAbort(e) || !current(id)) return;
			failed = true;
		}
		schedule();
	}

	async function restart() {
		if (retest === 'busy') return;
		const { id, signal } = begin();
		retest = 'busy';
		retryAfter = undefined;
		try {
			const next = await api.post('/network/check', { signal });
			if (!current(id)) return;
			check = next;
			failed = false;
			stopped = false;
			startedAt = Date.now();
			retest = 'idle';
		} catch (e) {
			if (isAbort(e) || !current(id)) return;
			if (e instanceof ApiError && e.code === 'rate_limited') {
				retest = 'limited';
				retryAfter = e.retryAfter;
			} else {
				retest = 'error';
			}
		}
		schedule();
	}

	return {
		get check() {
			return check;
		},
		/** The last read failed (and no earlier answer is shown as current). */
		get failed() {
			return failed;
		},
		/** Reading stopped at the deadline without an answer. */
		get stopped() {
			return stopped;
		},
		get retest() {
			return retest;
		},
		get retryAfter() {
			return retryAfter;
		},
		start: () => void read(),
		retestNow: () => void restart(),
		destroy() {
			gone = true;
			clearTimeout(timer);
			controller?.abort();
		}
	};
}
