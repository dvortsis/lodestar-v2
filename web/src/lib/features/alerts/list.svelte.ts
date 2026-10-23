// State of the Alerts list: the saved searches in the server's order, refreshed in place when an
// `alerts` event says counts changed. A refresh that fails keeps the rows it has (stale).
import { ApiError, isAbort, latest } from '$lib/api/client';
import { listSavedSearches, type SavedSearch } from './api';

export function createAlertsList() {
	const slot = latest();
	let items = $state<SavedSearch[]>();
	let error = $state<ApiError>();
	let loading = $state(false);
	let queued = false;

	async function load() {
		loading = true;
		try {
			items = await slot.run((signal) => listSavedSearches(signal));
			error = undefined;
		} catch (e) {
			if (isAbort(e)) return;
			error = e instanceof ApiError ? e : new ApiError(0, 'network', String(e));
		}
		loading = false;
	}

	return {
		get items() {
			return items;
		},
		get error() {
			return error;
		},
		get loading() {
			return loading;
		},
		load,
		/** Events delivered in one frame arrive in one task: they cause one request. */
		refreshSoon() {
			if (queued) return;
			queued = true;
			queueMicrotask(() => {
				queued = false;
				void load();
			});
		},
		/** The server's answer to a pause or resume replaces that row. */
		replace(updated: SavedSearch) {
			items = items?.map((s) => (s.id === updated.id ? updated : s));
		},
		remove(id: number) {
			items = items?.filter((s) => s.id !== id);
		},
		cancel: () => slot.cancel()
	};
}

export type AlertsList = ReturnType<typeof createAlertsList>;
