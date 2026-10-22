// The shell's view of the event stream: the last crawler and rebuild snapshots, the pill state and
// the connection kind. One instance for the app; `start()` subscribes and returns the stop.
import { events, type createEvents } from '$lib/state/events.svelte';
import { connectionKind, pillState, type CrawlerSnapshot, type RebuildSnapshot } from './status';

type EventsClient = Pick<ReturnType<typeof createEvents>, 'status' | 'error' | 'on'>;

export function createShellStatus(client: EventsClient = events) {
	let crawler = $state<CrawlerSnapshot | null>(null);
	let rebuild = $state<RebuildSnapshot | null>(null);
	let everLive = $state(false);

	const pill = $derived(pillState({ stream: client.status, crawler, rebuild }));
	const connection = $derived(
		connectionKind({ stream: client.status, error: client.error, everLive })
	);

	return {
		get pill() {
			return pill;
		},
		get connection() {
			return connection;
		},
		/** Call from an effect; it records that the stream has answered once. */
		observe() {
			if (client.status === 'live') everLive = true;
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
