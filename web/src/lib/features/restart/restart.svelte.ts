// One shared copy of `GET /system` for the banner, the pill and the keep-or-revert prompt, so they
// never disagree and Settings asks once. `refresh()` is cheap to call: calls in flight share one.
import { api } from '$lib/api/client';
import { pendingOf, type Pending, type System } from './logic';

function createRestartStore() {
	let system = $state.raw<System>();
	let inflight: Promise<void> | undefined;
	return {
		get system() {
			return system;
		},
		get pending(): Pending | null {
			return pendingOf(system);
		},
		refresh(signal?: AbortSignal): Promise<void> {
			return (inflight ??= api
				.get('/system', { signal })
				.then((s) => void (system = s))
				.catch(() => {})
				.finally(() => (inflight = undefined)));
		}
	};
}

export const restart = createRestartStore();
