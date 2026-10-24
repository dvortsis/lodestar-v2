// Discover's data: the first page of all three sections for a tab, then Newest's cursor pages.
// The server decides every count, section and Adult rule; this only holds what it sent.
import { ApiError, api, isAbort, latest, mustRestartPaging } from '$lib/api/client';
import type { components } from '$lib/api/schema';

export type SearchHit = components['schemas']['SearchHit'];
export type NewestSection = components['schemas']['NewestSection'];
export type HitGroup = components['schemas']['HitGroup'];

/** Answers faster than this show no skeleton (§10.4 "Refining"). */
export const SKELETON_DELAY_MS = 300;

export type Failure = 'unknown_type' | 'search_disabled' | 'other';

export function failureOf(e: unknown): Failure {
	if (e instanceof ApiError) {
		if (e.status === 400 && e.code === 'unknown_type') return 'unknown_type';
		if (e.status === 503 && e.code === 'search_disabled') return 'search_disabled';
	}
	return 'other';
}

export class Discover {
	/** The tab the data (or the request in flight) belongs to; null is All. */
	type = $state<string | null>(null);
	/** True from the first request until its answer or failure. */
	loading = $state(true);
	/** A request has taken longer than SKELETON_DELAY_MS with nothing to show for this tab. */
	slow = $state(false);
	newest = $state.raw<NewestSection | null>(null);
	hits = $state.raw<SearchHit[]>([]);
	rising = $state.raw<HitGroup | null>(null);
	resolved = $state.raw<HitGroup | null>(null);
	/** Why the tab has nothing to show (no rows were ever received for it). */
	failure = $state<Failure | null>(null);
	/** Paging failed: the rows above stay, Retry asks for the next page again. */
	moreFailed = $state(false);
	loadingMore = $state(false);
	/** A cursor expired mid-scroll: the list started over, and says so once. */
	restarted = $state(false);
	/** Counts every answered first page: the list's reset key and the live-region trigger. */
	loads = $state(0);
	/** True only for the page's first answered request (the §10.2 reveal). */
	firstData = $state(false);
	/** A tab change has been answered: the count may be announced. */
	switched = $state(false);

	/** The tab the held cursor belongs to (a cursor is bound to its tab, B13). */
	#pageType = $state<string | null>(null);

	/** Never while a first page is loading, or when the held cursor belongs to another tab. */
	readonly hasMore = $derived(
		this.newest !== null &&
			this.newest.next_cursor !== null &&
			!this.moreFailed &&
			!this.loading &&
			this.#pageType === this.type
	);

	#slot = latest();
	#pager = latest();
	#timer: ReturnType<typeof setTimeout> | undefined;
	#answeredOnce = false;
	#seq = 0;

	/** Shows `type`: restarts paging, keeps the previous rows up until the answer arrives. */
	show(type: string | null): void {
		const changed = type !== this.type || this.loads === 0;
		if (changed) this.restarted = false;
		this.type = type;
		void this.#first(changed);
	}

	retry(): void {
		void this.#first(false);
	}

	/** Retry after a failed page. */
	retryMore(): void {
		this.moreFailed = false;
		this.more();
	}

	/** The next Newest page for the current tab (never overlaps itself). */
	more(): void {
		const section = this.newest;
		if (!section?.next_cursor || this.loadingMore || !this.hasMore) return;
		const mine = this.#seq;
		this.loadingMore = true;
		this.moreFailed = false;
		const cursor = section.next_cursor;
		const type = this.type;
		this.#pager
			.run((signal) => api.get('/discover', { query: { type: type ?? undefined, cursor }, signal }))
			.then((res) => {
				if (mine !== this.#seq) return;
				this.hits = [...this.hits, ...res.newest.hits];
				this.newest = res.newest;
			})
			.catch((e: unknown) => {
				if (isAbort(e) || mine !== this.#seq) return;
				if (mustRestartPaging(e)) {
					this.restarted = true;
					void this.#first(false);
					return;
				}
				this.moreFailed = true;
			})
			.finally(() => {
				if (mine === this.#seq) this.loadingMore = false;
			});
	}

	destroy(): void {
		clearTimeout(this.#timer);
		this.#slot.cancel();
		this.#pager.cancel();
		this.#seq++;
	}

	async #first(changed: boolean): Promise<void> {
		const mine = ++this.#seq;
		const type = this.type;
		clearTimeout(this.#timer);
		this.#pager.cancel();
		this.failure = null;
		this.loading = true;
		this.loadingMore = false;
		this.moreFailed = false;
		if (changed) {
			this.slow = false;
			// Rows of another tab are never kept under this tab's heading once the wait is visible.
			this.#timer = setTimeout(() => {
				if (mine === this.#seq) this.slow = true;
			}, SKELETON_DELAY_MS);
		}
		try {
			const res = await this.#slot.run((signal) =>
				api.get('/discover', { query: { type: type ?? undefined }, signal })
			);
			if (mine !== this.#seq) return;
			this.newest = res.newest;
			this.#pageType = type;
			this.hits = res.newest.hits;
			this.rising = res.rising;
			this.resolved = res.recently_resolved;
			this.failure = null;
			this.firstData = !this.#answeredOnce;
			this.switched = this.#answeredOnce && changed;
			this.#answeredOnce = true;
			this.loads++;
		} catch (e) {
			if (isAbort(e) || mine !== this.#seq) return;
			this.failure = failureOf(e);
			// A failed refresh of the same tab keeps its rows (stale, with a Retry); another tab's rows
			// are never left under this tab, and a rejected type or search being off has none to keep.
			if (changed || this.failure !== 'other') this.#clear();
		} finally {
			if (mine === this.#seq) {
				clearTimeout(this.#timer);
				this.slow = false;
				this.loading = false;
			}
		}
	}

	#clear(): void {
		this.newest = null;
		this.hits = [];
		this.rising = null;
		this.resolved = null;
	}
}
