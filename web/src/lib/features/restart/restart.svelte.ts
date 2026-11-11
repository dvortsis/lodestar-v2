// One shared copy of `GET /system` for the banner, the pill and the keep-or-revert prompt, so they
// never disagree and Settings asks once. `refresh()` is cheap to call: calls in flight share one.
// It also holds the restart in progress, above any component that starts it, so a refresh that
// finds nothing pending any more cannot tear the full-page Restarting state down before it reloads.
import { api } from '$lib/api/client';
import { pendingOf, type Pending, type System, type Waiting } from './logic';

const hostname = () => (typeof location === 'undefined' ? '' : location.hostname);

function createRestartStore() {
	let system = $state.raw<System>();
	let waiting = $state.raw<Waiting | null>(null);
	let inflight: Promise<void> | undefined;
	return {
		get system() {
			return system;
		},
		get pending(): Pending | null {
			return pendingOf(system, hostname());
		},
		/** The restart in progress; null when none. */
		get waiting() {
			return waiting;
		},
		/** Starts the Restarting state; `null` ends it (tests, and a page that gave up). */
		begin(w: Waiting | null) {
			waiting = w;
		},
		/** Not while restarting: the server is going away, and the Restarting page checks it itself. */
		refresh(signal?: AbortSignal): Promise<void> {
			if (waiting) return Promise.resolve();
			return (inflight ??= api
				.get('/system', { signal })
				.then((s) => void (waiting || (system = s)))
				.catch(() => {})
				.finally(() => (inflight = undefined)));
		}
	};
}

export const restart = createRestartStore();
