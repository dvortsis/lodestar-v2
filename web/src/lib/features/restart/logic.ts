// The restart banner and flow (§10.10.6) as pure decisions: what the banner offers, which errors
// stop the flow, how the wait for the server backs off, and the address keep-or-revert prompt. The
// components fetch and render; nothing here touches the network except `waitForServer`'s injected
// probe, so the tests drive it with a fake clock.
import { ApiError } from '$lib/api/client';
import type { components } from '$lib/api/schema';

export type System = components['schemas']['SystemInfo'];

export interface Pending {
	/** Saved settings that wait for a restart, as `section.field`. */
	fields: string[];
	/** Whether the server can restart itself: the button shows only then. */
	canRestart: boolean;
	/** How to restart instead, when it can't. */
	hint: string | null;
	/** About how long a restart pauses crawling, from the server. */
	estimateSeconds: number;
	/** Where the UI will be after the restart, when the HTTP address changed. */
	newUrl: string | null;
}

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

/** What the full-page Restarting state waits for. */
export interface Waiting {
	/** Where the UI will be when the HTTP address changed; null to stay on this address. */
	target: string | null;
	/** The run that is ending (`GET /system` `started_at`); null when unknown. */
	startedAt: number | null;
	estimateSeconds: number;
}

/** What the banner shows: nothing, or the pending changes. */
export function pendingOf(system: System | undefined, hostname = ''): Pending | null {
	if (!system || system.restart_required.length === 0) return null;
	const change = system.address_change;
	return {
		fields: system.restart_required,
		canRestart: system.restart.available,
		hint: system.restart.hint,
		estimateSeconds: system.restart.estimated_seconds,
		newUrl:
			system.restart_required.includes('network.http_bind') && change?.state === 'awaiting_restart'
				? usableUrl(change.url, hostname)
				: null
	};
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
