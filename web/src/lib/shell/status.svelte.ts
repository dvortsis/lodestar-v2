// The shell's view of the event stream: the last crawler and rebuild snapshots, the pill state and
// the connection kind. One instance for the app; `start()` subscribes and returns the stop.
import { onSessionOk } from '$lib/api/client';
import { events, type createEvents } from '$lib/state/events.svelte';
import {
	connectionKind,
	pillState,
	type CrawlerSnapshot,
	type ProgressOverlay,
	type RebuildSnapshot
} from './status';

type EventsClient = Pick<ReturnType<typeof createEvents>, 'status' | 'error' | 'on'>;

/** A cheap, public request: any HTTP answer below 500 proves the server is there. */
async function probeServer(): Promise<boolean> {
	try {
		const res = await fetch('/api/v1/auth', { credentials: 'same-origin' });
		return res.status < 500;
	} catch {
		return false;
	}
}

export function createShellStatus(
	client: EventsClient = events,
	probe: () => Promise<boolean> = probeServer
) {
	let crawler = $state<CrawlerSnapshot | null>(null);
	let rebuild = $state<RebuildSnapshot | null>(null);
	let progress = $state<ProgressOverlay>(null);
	let everLive = $state(false);
	// The stream is the only thing the shell opens itself, and proxies or antivirus tools can hold
	// just that back. Any REST success, or a probe that gets an answer, says the server is there.
	let restOk = $state(false);
	let probing = false;
	onSessionOk(() => (restOk = true));

	const pill = $derived(pillState({ stream: client.status, crawler, rebuild, progress }));
	const connection = $derived(
		connectionKind({ stream: client.status, error: client.error, everLive, restOk })
	);

	return {
		get pill() {
			return pill;
		},
		get connection() {
			return connection;
		},
		/** The last rebuild snapshot (the search results' rebuild banner reads it). */
		get rebuild() {
			return rebuild;
		},
		/** Call from an effect; it records that the stream has answered once. */
		observe() {
			if (client.status === 'live') everLive = true;
			// The stream failed before it ever answered: ask something else before calling it down.
			if (client.status === 'offline' && client.error && !everLive && !restOk && !probing) {
				probing = true;
				void probe().then((ok) => {
					probing = false;
					if (ok) restOk = true;
				});
			}
		},
		/** The server's progress facts (`progress.ts` polls them once the chrome is up). */
		setProgress(p: ProgressOverlay) {
			progress = p;
		},
		/** Subscribes to `crawler` and `rebuild` (one stream, shared with other consumers). */
		start(): () => void {
			const offs = [
				client.on('crawler', (d) => (crawler = d)),
				client.on('rebuild', (d) => (rebuild = d))
			];
			return () => offs.forEach((off) => off());
		}
	};
}

export const shellStatus = createShellStatus();
