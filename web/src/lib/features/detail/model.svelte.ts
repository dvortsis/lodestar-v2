// Every open revalidates visibility. Cached metadata is never displayed before that succeeds.
import { ApiError, api, isAbort, latest } from '$lib/api/client';
import {
	cacheDetail,
	cachedHit,
	cacheHit,
	detailGeneration,
	forgetDetail,
	onDetailInvalidated,
	type SearchHit,
	type TorrentDetail
} from './cache';
import { adoptPrefetch } from './prefetch';
import { setLoadedHash } from './target';
import { watchDetailVisibility } from './visibility';

export type Status = 'loading' | 'ready' | 'error';

export class DetailModel {
	hash = $state('');
	// Raw: replaced wholesale, never mutated, and the file list goes to a Worker (a proxy can't be cloned).
	detail = $state.raw<TorrentDetail | null>(null);
	hit = $state.raw<SearchHit | null>(null);
	status = $state<Status>('loading');
	/** The hit lookup has finished (found or not): the title won't change again. */
	hitDone = $state(false);
	error = $state<ApiError | null>(null);
	/** The title shown before and after load: resolved title, else the raw name. */
	title = $derived(this.hit?.title ?? this.detail?.name ?? this.hit?.name ?? '');
	private slot = latest();
	private hitSlot = latest();
	private generation = 0;
	private off: (() => void) | undefined;

	/** (Re)load `hash`. Cheap to call again: the previous request is cancelled. */
	load(hash: string) {
		watchDetailVisibility();
		this.off ??= onDetailInvalidated((revalidate) => {
			const current = this.hash;
			this.clear();
			if (revalidate && current) this.load(current);
		});
		this.clear();
		this.hash = hash;
		void this.fetchDetail(hash, this.generation, detailGeneration());
	}

	retry() {
		if (!this.hash) return;
		this.load(this.hash);
	}

	destroy() {
		this.off?.();
		this.off = undefined;
		this.clear();
		this.hash = '';
	}

	private clear() {
		this.generation++;
		this.slot.cancel();
		this.hitSlot.cancel();
		this.detail = null;
		setLoadedHash(null);
		this.hit = null;
		this.hitDone = false;
		this.error = null;
		this.status = 'loading';
	}

	private current(generation: number, visibility: number) {
		return generation === this.generation && visibility === detailGeneration();
	}

	private async fetchDetail(hash: string, generation: number, visibility: number) {
		try {
			const adopted = adoptPrefetch(hash, visibility);
			// A prefetch from this visibility generation is the revalidation; an aborted one
			// (a newer hover) falls back to a fetch of our own.
			const fetchOwn = () =>
				this.slot.run((signal) => api.get('/torrents/{hash}', { path: { hash }, signal }));
			const detail =
				adopted === undefined
					? await fetchOwn()
					: await Promise.resolve(adopted).catch((e) => {
							if (!isAbort(e)) throw e;
							return this.current(generation, visibility) ? fetchOwn() : undefined;
						});
			if (!detail) return;
			if (!this.current(generation, visibility)) return;
			cacheDetail(hash, detail);
			this.detail = detail;
			setLoadedHash(hash); // `s` waits for the torrent to load
			const hit = cachedHit(hash) ?? null;
			this.hit = hit;
			this.hitDone = !!hit;
			this.status = 'ready';
			if (!hit) void this.fetchHit(hash, generation, visibility);
		} catch (e) {
			if (isAbort(e) || !this.current(generation, visibility)) return;
			forgetDetail(hash);
			this.detail = null;
			this.hit = null;
			this.error =
				e instanceof ApiError ? e : new ApiError(0, 'network', e instanceof Error ? e.message : '');
			this.status = 'error';
		}
	}

	// Interim until TorrentDetail carries the summary fields (metrics §7.1 "planned on detail").
	private async fetchHit(hash: string, generation: number, visibility: number) {
		try {
			const res = await this.hitSlot.run((signal) =>
				api.get('/search', { query: { q: `hash:${hash}`, limit: 1, hidden: true }, signal })
			);
			if (!this.current(generation, visibility)) return;
			const first = res.hits[0];
			if (first && first.hash === hash) {
				cacheHit(hash, first);
				this.hit = first;
			}
			this.hitDone = true;
		} catch (e) {
			if (!this.current(generation, visibility)) return;
			if (e instanceof ApiError && [401, 403, 404].includes(e.status)) {
				forgetDetail(hash);
				this.detail = null;
				setLoadedHash(null);
				this.hit = null;
				this.error = e;
				this.status = 'error';
			}
			if (!isAbort(e)) this.hitDone = true;
			// The header and rows fall back to their null states; the detail error is the one shown.
		}
	}
}
