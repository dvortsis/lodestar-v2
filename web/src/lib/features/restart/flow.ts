// What the restart flow needs once it is in use: the keep-or-revert prompt's numbers, the 409
// messages and the wait for the server. Kept apart from `logic.ts`, which the Settings banner loads
// on every Settings screen, so none of this is in a first view.
import { ApiError } from '$lib/api/client';
import type { System } from './logic';

/**
 * The server can't name its own address when it binds to every interface: it sends a placeholder
 * (`http://<this host's address>:9000/`) or `0.0.0.0`. The browser knows the host it reached it by.
 */
export function usableUrl(url: string, hostname: string): string {
	if (!hostname) return url;
	const named = url.replace(/<[^>]*>/, hostname.includes(':') ? `[${hostname}]` : hostname);
	try {
		const u = new URL(named);
		if (u.hostname !== '0.0.0.0' && u.hostname !== '[::]') return named;
		u.hostname = hostname.includes(':') ? `[${hostname}]` : hostname;
		return u.href;
	} catch {
		return named;
	}
}

export interface AddressPrompt {
	url: string;
	previousUrl: string;
	/** Whole seconds until the server reverts by itself; 0 once past. */
	secondsLeft: number;
}

/** "Keep this address?": shown while the server serves the new address and waits to hear. */
export function addressPromptOf(
	system: System | undefined,
	nowMs: number,
	hostname = ''
): AddressPrompt | null {
	const change = system?.address_change;
	if (!change || change.state !== 'awaiting_confirmation') return null;
	const left = change.deadline === null ? 0 : Math.ceil(change.deadline - nowMs / 1000);
	return {
		url: usableUrl(change.url, hostname),
		previousUrl: usableUrl(change.previous_url, hostname),
		secondsLeft: Math.max(0, left)
	};
}

export type RestartFailure =
	| { kind: 'unavailable'; message: string }
	| { kind: 'failed'; message: string }
	| { kind: 'error'; message: string };

/** `409 restart_unavailable` and `restart_failed` show the server's message and stop. */
export function failureOf(e: unknown): RestartFailure {
	if (e instanceof ApiError && e.status === 409) {
		if (e.code === 'restart_unavailable') return { kind: 'unavailable', message: e.message };
		if (e.code === 'restart_failed') return { kind: 'failed', message: e.message };
	}
	return { kind: 'error', message: e instanceof Error ? e.message : String(e) };
}

/** Not back within this, the page says what to check (§10.10.6). */
export const GIVE_UP_MS = 120_000;
/** The first probe waits this long: the server answers the 202 before it shuts down. */
export const FIRST_PROBE_MS = 1_000;

/** Backoff between probes: 1 s, growing by half each time, never above 5 s. */
export function backoffMs(attempt: number): number {
	return Math.min(1_000 * 1.5 ** attempt, 5_000);
}

export interface WaitOptions {
	/** True when `/healthz` answered. */
	probe: () => Promise<boolean>;
	/** Whether an answering server is a new run, not the old one still shutting down. */
	isNewRun?: () => Promise<boolean>;
	now: () => number;
	sleep: (ms: number) => Promise<void>;
	signal?: AbortSignal;
	giveUpMs?: number;
	/**
	 * With no `isNewRun`, an answer before the server was seen down only counts after this long: the
	 * old process may still be shutting down on the same port.
	 */
	settleMs?: number;
}

/** Waits for the server to come back: `up`, `timeout`, or `aborted`. */
export async function waitForServer(o: WaitOptions): Promise<'up' | 'timeout' | 'aborted'> {
	const started = o.now();
	const limit = o.giveUpMs ?? GIVE_UP_MS;
	let sawDown = false;
	await o.sleep(FIRST_PROBE_MS);
	for (let attempt = 0; ; attempt++) {
		if (o.signal?.aborted) return 'aborted';
		if (await o.probe()) {
			// Answering, but maybe the old process: it counts once we've seen it down, or when
			// something says this is a new run.
			if (sawDown) return 'up';
			if (o.isNewRun) {
				if (await o.isNewRun().catch(() => false)) return 'up';
			} else if (o.now() - started >= (o.settleMs ?? 0)) return 'up';
		} else sawDown = true;
		if (o.now() - started >= limit) return 'timeout';
		await o.sleep(backoffMs(attempt));
	}
}

/** Waits until the probe stops answering: `down`, `timeout`, or `aborted`. */
export async function waitUntilDown(
	o: Pick<WaitOptions, 'probe' | 'now' | 'sleep' | 'signal' | 'giveUpMs'>
): Promise<'down' | 'timeout' | 'aborted'> {
	const started = o.now();
	const limit = o.giveUpMs ?? GIVE_UP_MS;
	for (let attempt = 0; ; attempt++) {
		if (o.signal?.aborted) return 'aborted';
		if (!(await o.probe())) return 'down';
		if (o.now() - started >= limit) return 'timeout';
		await o.sleep(backoffMs(attempt));
	}
}

/** `url` as an http(s) address, or null: anything else is never navigated to or linked. */
export function parseTarget(url: string | null): URL | null {
	if (!url) return null;
	try {
		const u = new URL(url);
		return u.protocol === 'http:' || u.protocol === 'https:' ? u : null;
	} catch {
		return null;
	}
}

/** Whether `url` is a different origin from this page: the CSP only lets the page fetch itself. */
export function isOtherOrigin(url: string | null, here: string): boolean {
	const u = parseTarget(url);
	return u !== null && u.origin !== here;
}
