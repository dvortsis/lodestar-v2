// State of one alert's page: the saved search, its alerts newest first (paged by cursor), the
// "N new · Show" banner and the read mark. Counts are the server's: `opened` is the unread count
// as this visit began, `fresh` the server's count after an event, never a sum of events.
import { ApiError, isAbort, latest, mustRestartPaging } from '$lib/api/client';
import { getSavedSearch, listAlerts, markAlertsRead, type AlertRow, type SavedSearch } from './api';
import { dividerBefore } from './since';

const toError = (e: unknown) => (e instanceof ApiError ? e : new ApiError(0, 'network', String(e)));

export function createAlertDetail(id: number) {
	const slot = latest();
	const searchSlot = latest();
	const moreSlot = latest();
	/** What failed last, so Retry asks for that. */
	let failed: 'load' | 'search' = 'load';
	let search = $state<SavedSearch>();
	let rows = $state<AlertRow[]>([]);
	let cursor = $state<string | null>(null);
	let loading = $state(true);
	let loadingMore = $state(false);
	/** Loading the first page (or the search) failed: nothing to show yet, or a stale view. */
	let error = $state<ApiError>();
	/** Loading more failed: the rows stay, paging stops, a Retry shows. */
	let moreError = $state<ApiError>();
	let opened = $state<number>();
	let fresh = $state(0);
	let mark = $state<{ at: number; before: number } | null>(null);

	function place() {
		mark = dividerBefore(rows);
	}

	/** Marks everything up to the newest row you have seen, once per load. Failure is silent. */
	function markRead() {
		const top = rows[0];
		if (!top || top.read) return;
		void markAlertsRead(id, top.id).catch(() => {});
	}

	/** The search and its first page together, then the read mark. */
	async function load(): Promise<void> {
		loading = true;
		moreSlot.cancel(); // a page of the old list must not land on the new one
		loadingMore = false;
		try {
			const [s, page] = await slot.run((signal) =>
				Promise.all([getSavedSearch(id, signal), listAlerts(id, null, signal)])
			);
			search = s;
			opened = s.unread;
			fresh = 0;
			rows = page.items;
			cursor = page.next_cursor;
			error = undefined;
			moreError = undefined;
			place();
			loading = false;
			markRead();
		} catch (e) {
			if (isAbort(e)) return;
			failed = 'load';
			error = toError(e);
			if (error.code === 'saved_search_not_found') search = undefined;
			loading = false;
		}
	}

	async function loadMore(): Promise<void> {
		if (!cursor || loadingMore || loading) return;
		loadingMore = true;
		const requested = cursor;
		try {
			const page = await moreSlot.run((signal) => listAlerts(id, requested, signal));
			// A local, never rendered from: a plain Set is what's meant.
			// eslint-disable-next-line svelte/prefer-svelte-reactivity
			const seen = new Set(rows.map((r) => r.id));
			rows = [...rows, ...page.items.filter((r) => !seen.has(r.id))];
			cursor = page.next_cursor;
			moreError = undefined;
			place();
		} catch (e) {
			if (isAbort(e)) return;
			if (mustRestartPaging(e)) {
				return load();
			}
			// Stop paging (the list would ask again at once) and offer a Retry.
			retryCursor = requested;
			cursor = null;
			moreError = toError(e);
		} finally {
			loadingMore = false;
		}
	}
	let retryCursor: string | null = null;
	let queued = false;

	return {
		get search() {
			return search;
		},
		get rows() {
			return rows;
		},
		get hasMore() {
			return cursor !== null;
		},
		get loading() {
			return loading;
		},
		get loadingMore() {
			return loadingMore;
		},
		get error() {
			return error;
		},
		get moreError() {
			return moreError;
		},
		get opened() {
			return opened;
		},
		get fresh() {
			return fresh;
		},
		get mark() {
			return mark;
		},
		load,
		loadMore,
		retryMore() {
			cursor = retryCursor;
			moreError = undefined;
			return loadMore();
		},
		/** An event for this search: ask the server what is unread now. */
		async refreshSearch(): Promise<void> {
			try {
				const s = await searchSlot.run((signal) => getSavedSearch(id, signal));
				search = s;
				fresh = s.unread;
				error = undefined;
			} catch (e) {
				if (isAbort(e)) return;
				failed = 'search';
				error = toError(e); // the old counts stay on screen
				if (error.code === 'saved_search_not_found') search = undefined;
			}
		},
		/** Retry whatever failed last. */
		retry() {
			return failed === 'search' ? this.refreshSearch() : load();
		},
		/** Events delivered in one frame arrive in one task: they cause one request. */
		refreshSoon() {
			if (queued) return;
			queued = true;
			queueMicrotask(() => {
				queued = false;
				void this.refreshSearch();
			});
		},
		/** "Show": the first page again, the mark placed from the new flags, then read. */
		show: load,
		replace(updated: SavedSearch) {
			search = updated;
		},
		cancel() {
			slot.cancel();
			searchSlot.cancel();
			moreSlot.cancel();
		}
	};
}

export type AlertDetail = ReturnType<typeof createAlertDetail>;
