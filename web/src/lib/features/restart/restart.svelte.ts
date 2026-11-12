// One shared copy of `GET /system` for the banner, the pill and the keep-or-revert prompt, so they
// never disagree and Settings asks once. `refresh()` is cheap to call: calls in flight share one.
// It also holds the restart in progress, above any component that starts it, so a refresh that
// finds nothing pending any more cannot tear the full-page Restarting state down before it reloads.
import { api } from '$lib/api/client';
import { pendingOf, type Pending, type System, type Waiting } from './logic';

function createRestartStore() {
	let system = $state.raw<System>();
	let waiting = $state.raw<Waiting | null>(null);
	let inflight: Promise<boolean> | undefined;
	let inflightGeneration = 0;
	// Bumped by every answer that is newer than any read in flight (Keep, Go back, a restart that
	// began, a Settings save): a `/system` read that started before it must not write its older
	// state back.
	let generation = 0;
	/**
	 * Whether the server answered. Not while restarting: the server is going away, and the
	 * Restarting page checks it itself. An answer that was asked for before a newer one (see
	 * `generation`) is dropped, and a read in flight from before it is not joined.
	 */
	function refresh(signal?: AbortSignal): Promise<boolean> {
		if (waiting) return Promise.resolve(true);
		if (inflight && inflightGeneration === generation) return inflight;
		const mine = generation;
		const read: Promise<boolean> = api
			.get('/system', { signal })
			.then((s) => {
				if (!waiting && mine === generation) system = s;
				return true;
			})
			.catch(() => false)
			.finally(() => {
				if (inflight === read) inflight = undefined;
			});
		inflight = read;
		inflightGeneration = mine;
		return read;
	}
	return {
		get system() {
			return system;
		},
		get pending(): Pending | null {
			return pendingOf(system);
		},
		/** The restart in progress; null when none. */
		get waiting() {
			return waiting;
		},
		/** The server's answer to Keep or Revert is the new state: use it, don't ask again. */
		setAddressChange(change: System['address_change']) {
			generation++;
			if (system) system = { ...system, address_change: change };
		},
		/** Starts the Restarting state; `null` ends it (tests, and a page that gave up). */
		begin(w: Waiting | null) {
			generation++;
			waiting = w;
		},
		refresh,
		/**
		 * A Settings save answered. A read still in flight left before the save, so it may carry
		 * the old `restart_required`: drop it, and ask again now instead of at the next poll.
		 */
		saved(): Promise<boolean> {
			generation++;
			return refresh();
		}
	};
}

export const restart = createRestartStore();
