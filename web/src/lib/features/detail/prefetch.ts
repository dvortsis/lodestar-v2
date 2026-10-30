// Hover and focus warm-up for the detail drawer (§10.1). The model adopts a prefetch made in the
// same visibility generation, so hover then open is one request; the request counts as the
// revalidation, and a visibility change drops it.
import { api, isAbort } from '$lib/api/client';
import {
	cacheDetail,
	cachedDetail,
	detailGeneration,
	forgetDetail,
	onDetailInvalidated,
	type TorrentDetail
} from './cache';
import { watchDetailVisibility } from './visibility';

const IDLE_MS = 150;
let timer: ReturnType<typeof setTimeout> | undefined;
let inflight: AbortController | undefined;

interface Run {
	hash: string;
	generation: number;
	promise: Promise<TorrentDetail>;
	/** Set once the request has settled, so a later open can still adopt the result. */
	done?: TorrentDetail;
}
let run: Run | undefined;
let pendingHash: string | undefined;

export function cancelDetailPrefetch() {
	clearTimeout(timer);
	timer = undefined;
	pendingHash = undefined;
	inflight?.abort();
	inflight = undefined;
	run = undefined;
}
onDetailInvalidated(cancelDetailPrefetch);

/**
 * Take over the prefetch of `hash` made under visibility generation `generation`: the finished
 * detail, or the request still in flight. Returns undefined when there is none (the caller
 * fetches itself); a timer still waiting for `hash` is dropped for the same reason.
 */
export function adoptPrefetch(
	hash: string,
	generation: number
): TorrentDetail | Promise<TorrentDetail> | undefined {
	if (pendingHash === hash) {
		clearTimeout(timer);
		timer = undefined;
		pendingHash = undefined;
	}
	if (run?.hash !== hash || run.generation !== generation || generation !== detailGeneration())
		return undefined;
	return run.done ?? run.promise;
}

/**
 * Warm the cache for `hash` after 150 ms of idle. A newer call cancels a pending timer and an
 * in-flight request, so only the torrent the pointer or focus rests on is fetched.
 */
export function prefetchDetail(hash: string) {
	watchDetailVisibility();
	cancelDetailPrefetch();
	if (cachedDetail(hash)) return;
	const generation = detailGeneration();
	pendingHash = hash;
	timer = setTimeout(() => {
		pendingHash = undefined;
		const ctl = (inflight = new AbortController());
		const mine: Run = {
			hash,
			generation,
			promise: api.get('/torrents/{hash}', { path: { hash }, signal: ctl.signal })
		};
		run = mine;
		mine.promise.then(
			(detail) => {
				if (ctl.signal.aborted || generation !== detailGeneration()) return;
				mine.done = detail;
				cacheDetail(hash, detail);
			},
			(e) => {
				if (run === mine) run = undefined;
				if (!isAbort(e) && !ctl.signal.aborted && generation === detailGeneration())
					forgetDetail(hash);
			}
		);
		void mine.promise
			.finally(() => {
				if (inflight === ctl) inflight = undefined;
			})
			.catch(() => {});
	}, IDLE_MS);
}
