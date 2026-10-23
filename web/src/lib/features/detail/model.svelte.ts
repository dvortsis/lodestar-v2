// The data behind one open torrent: the detail body (prefetch cache, else a latest-wins request)
// and, for the fields detail doesn't carry yet, the search hit (cached from Search, else a hash
// lookup). A slow response for an earlier hash never overwrites a newer one.
import { ApiError, api, isAbort, latest } from '$lib/api/client';
import {
	cacheDetail,
	cachedDetail,
	cachedHit,
	cacheHit,
	type SearchHit,
	type TorrentDetail
} from './cache';

export type Status = 'loading' | 'ready' | 'error';

export class DetailModel {
	hash = $state('');
	// Raw: replaced wholesale, never mutated, and the file list goes to a Worker (a proxy can't be cloned).
	detail = $state.raw<TorrentDetail | null>(null);
	hit = $state.raw<SearchHit | null>(null);
	status = $state<Status>('loading');
	error = $state<ApiError | null>(null);
	/** The title shown before and after load: resolved title, else the raw name. */
	title = $derived(this.hit?.title ?? this.detail?.name ?? this.hit?.name ?? '');
	private slot = latest();
	private hitSlot = latest();

	/** (Re)load `hash`. Cheap to call again: the previous request is cancelled. */
	load(hash: string) {
		// A swap must drop the earlier torrent's in-flight requests even when this one is cached.
		this.slot.cancel();
		this.hitSlot.cancel();
		this.hash = hash;
		this.error = null;
		// Plain locals: reading this.hit here would make a calling $effect depend on it.
		const hit = cachedHit(hash) ?? null;
		const cached = cachedDetail(hash);
		this.hit = hit;
		this.detail = cached ?? null;
		this.status = cached ? 'ready' : 'loading';
		if (!cached) void this.fetchDetail(hash);
		if (!hit) void this.fetchHit(hash);
	}

	retry() {
		if (!this.hash) return;
		this.error = null;
		this.status = 'loading';
		void this.fetchDetail(this.hash);
		if (!this.hit) void this.fetchHit(this.hash);
	}

	destroy() {
		this.slot.cancel();
		this.hitSlot.cancel();
	}

	private async fetchDetail(hash: string) {
		try {
			const detail = await this.slot.run((signal) =>
				api.get('/torrents/{hash}', { path: { hash }, signal })
			);
			cacheDetail(hash, detail);
			this.detail = detail;
			this.status = 'ready';
		} catch (e) {
			if (isAbort(e)) return;
			this.detail = null;
			this.error =
				e instanceof ApiError ? e : new ApiError(0, 'network', e instanceof Error ? e.message : '');
			this.status = 'error';
		}
	}

	// Interim until TorrentDetail carries the summary fields (metrics §7.1 "planned on detail").
	private async fetchHit(hash: string) {
		try {
			const res = await this.hitSlot.run((signal) =>
				api.get('/search', { query: { q: `hash:${hash}`, limit: 1, hidden: true }, signal })
			);
			const first = res.hits[0];
			if (first && first.hash === hash) {
				cacheHit(hash, first);
				this.hit = first;
			}
		} catch {
			// The header and rows fall back to their null states; the detail error is the one shown.
		}
	}
}
