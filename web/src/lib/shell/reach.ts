// Telling "the server is not there" from "the server said no", and waiting for it to come back.
// Kit shows +error.svelte with status 500 both when a route's code or data could not be fetched
// because nothing answered, and when the server really failed. Only the first is a connection
// problem: ask the server's health endpoint. Any HTTP answer, whatever its status, means it is up.

export const FIRST_MS = 1_000;
export const CAP_MS = 30_000;

/** 1 s, 2 s, 4 s … never above 30 s. */
export function backoffMs(attempt: number): number {
	return Math.min(FIRST_MS * 2 ** attempt, CAP_MS);
}

/** True when the server answered at all; false when the request itself failed. */
export async function serverAnswers(
	fetcher: typeof fetch = (input, init) => fetch(input, init),
	signal?: AbortSignal
): Promise<boolean> {
	try {
		await fetcher('/healthz', { cache: 'no-store', signal });
		return true;
	} catch {
		return false;
	}
}

export interface Watch {
	/** Probe now, instead of waiting for the timer. */
	retryNow: () => void;
	stop: () => void;
}

export interface WatchOptions {
	probe: () => Promise<boolean>;
	/** The server answered. */
	onUp: () => void;
	/** `checking` while a probe is out; `waiting` with the delay before the next one. */
	onState: (state: { phase: 'checking' } | { phase: 'waiting'; delayMs: number }) => void;
	setTimer?: (fn: () => void, ms: number) => unknown;
	clearTimer?: (id: unknown) => void;
}

/** Probes with backoff until the server answers. Starts by waiting, not probing: the caller has
 *  just seen it fail. */
export function watchServer(o: WatchOptions): Watch {
	const setTimer = o.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
	const clearTimer = o.clearTimer ?? ((id) => clearTimeout(id as ReturnType<typeof setTimeout>));
	let attempt = 0;
	let timer: unknown;
	let stopped = false;
	let busy = false;

	const wait = () => {
		const delayMs = backoffMs(attempt++);
		o.onState({ phase: 'waiting', delayMs });
		timer = setTimer(run, delayMs);
	};
	const run = async () => {
		if (stopped || busy) return;
		busy = true;
		clearTimer(timer);
		o.onState({ phase: 'checking' });
		const up = await o.probe();
		busy = false;
		if (stopped) return;
		if (up) {
			stopped = true;
			o.onUp();
		} else wait();
	};
	wait();
	return {
		retryNow: () => void run(),
		stop: () => {
			stopped = true;
			clearTimer(timer);
		}
	};
}
