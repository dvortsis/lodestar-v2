// Adding `.torrent` files by hand (UP-2, UI review 02 §5.3): the checks made before a file is
// sent, the one request per file (`POST /torrents/upload`, UP-0), and what each answer means.
// No Svelte here, so every rule is unit-tested on its own.
import {
	type ApiError,
	abortError,
	errorFromResponse,
	handleUnauthorized,
	isAbort,
	sessionOk
} from '$lib/api/client';

/** The largest file the server takes: 8 MiB (`upload::MAX_BYTES`, plan §5). */
export const MAX_BYTES = 8 * 1024 * 1024;
/**
 * Files per pick, at most: the server's rate-limit burst (`upload::RATE_BURST`), so a whole pick
 * goes through on a fresh allowance instead of failing half-way with "too many uploads".
 */
export const MAX_FILES = 30;
/** The one content type the route takes, exactly (`upload::CONTENT_TYPE`); anything else is a 415. */
export const CONTENT_TYPE = 'application/x-bittorrent';
const PATH = '/api/v1/torrents/upload';

/** Where a file stands. `sending` and `queued` are on their way; the rest are final until a retry. */
export type State =
	| 'queued'
	| 'sending'
	| 'added'
	| 'malformed'
	| 'held'
	| 'refused'
	| 'too_large'
	| 'proxy_too_large'
	| 'turned_away'
	| 'rate_limited'
	| 'unavailable'
	| 'failed'
	| 'stopped';

export interface Item {
	/** Stable within a screen's life, for keyed lists and focus. */
	id: number;
	file: File;
	state: State;
	/** On `added`: the info hash (40 lowercase hex characters), checked before any link uses it. */
	hash?: string;
	/** On `added`: the file says `private=1`. */
	private?: boolean;
	/** On `rate_limited`: seconds the server asked to wait. */
	wait?: number;
	/** On `rate_limited`: true until `wait` has passed, so Retry can't earn another 429 at once. */
	hold?: boolean;
}

/** States a press of Retry can change: the server or the connection failed, never the file. */
export const RETRYABLE: ReadonlySet<State> = new Set([
	'rate_limited',
	'unavailable',
	'failed',
	'stopped'
]);

/** What the server, or a check before sending, made of a file. */
export type Outcome =
	| { state: 'added'; hash: string; private: boolean }
	| { state: Exclude<State, 'added' | 'queued' | 'sending' | 'rate_limited'> }
	| { state: 'rate_limited'; wait?: number };

/**
 * The check made before a file is sent, so the person learns the limit without waiting on the
 * server: over 8 MiB is too large, and an empty file or one not named `.torrent` isn't a torrent
 * file. Undefined means send it; the server judges its contents.
 */
export function precheck(file: Pick<File, 'name' | 'size'>): Outcome | undefined {
	if (file.size > MAX_BYTES) return { state: 'too_large' };
	if (file.size === 0 || !/\.torrent$/i.test(file.name)) return { state: 'malformed' };
	return undefined;
}

const HASH = /^[0-9a-f]{40}$/;

/** A `202`'s body, trusted only as far as its shape: a hash that isn't 40 hex digits is dropped. */
export function accepted(body: unknown): Outcome {
	const b = (typeof body === 'object' && body !== null ? body : {}) as Record<string, unknown>;
	const hash = typeof b.info_hash === 'string' ? b.info_hash.toLowerCase() : '';
	return { state: 'added', hash: HASH.test(hash) ? hash : '', private: b.private === true };
}

/** A refusal, by its stable code (never its message, which is the server's English). */
export function refused(e: ApiError): Outcome {
	switch (e.code) {
		case 'malformed_torrent':
			return { state: 'malformed' };
		case 'held':
			return { state: 'held' };
		case 'refused':
			return { state: 'refused' };
		case 'body_too_large':
			return { state: 'too_large' };
		case 'rate_limited':
			return { state: 'rate_limited', wait: e.retryAfter };
		case 'uploads_unavailable':
			return { state: 'unavailable' };
		case 'cross_origin':
		case 'unsupported_media_type':
			return { state: 'turned_away' };
	}
	// Answers a retry can't change. A proxy in front of Lodestar may cap bodies below 8 MiB
	// (nginx's default is 1 MiB) and answer 413 with its own page, so no limit is named. A 403 or
	// 415 without our code is a proxy's refusal too.
	switch (e.status) {
		case 413:
			return { state: 'proxy_too_large' };
		case 403:
		case 415:
			return { state: 'turned_away' };
		default:
			return { state: 'failed' };
	}
}

/**
 * Sends one file as the raw request body, the only form the route takes: no multipart, so no file
 * name, path or form field ever leaves the browser, only the file's bytes. Headers are the fixed
 * content type and `Accept`; the browser adds `Origin` and `Sec-Fetch-Site`, which the server's
 * same-origin check reads, and the session cookie (same-origin only). A redirect is refused rather
 * than followed with the file. A sign-in that lapsed goes to the shell's sign-in handler, as every
 * other request does. Rejects only when `signal` aborts.
 */
export async function send(file: Blob, signal?: AbortSignal): Promise<Outcome> {
	let res: Response;
	try {
		res = await fetch(PATH, {
			method: 'POST',
			headers: { 'Content-Type': CONTENT_TYPE, Accept: 'application/json' },
			body: file,
			credentials: 'same-origin',
			cache: 'no-store',
			redirect: 'error',
			signal
		});
	} catch (e) {
		if (isAbort(e) || signal?.aborted) throw abortError();
		return { state: 'failed' };
	}
	if (!res.ok) {
		const err = await errorFromResponse(res);
		handleUnauthorized(err, '/torrents/upload');
		return refused(err);
	}
	sessionOk('/torrents/upload');
	try {
		return accepted(await res.json());
	} catch (e) {
		if (isAbort(e) || signal?.aborted) throw abortError();
		// Taken, but the answer didn't say which torrent: added, with no link.
		return { state: 'added', hash: '', private: false };
	}
}

/** Counts by state, for the summary line. Only states with at least one file are listed. */
export function tally(items: readonly Pick<Item, 'state'>[]): Partial<Record<State, number>> {
	const out: Partial<Record<State, number>> = {};
	for (const i of items) out[i.state] = (out[i.state] ?? 0) + 1;
	return out;
}
