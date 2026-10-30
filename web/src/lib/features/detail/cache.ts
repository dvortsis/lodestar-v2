// Small in-memory caches for the detail feature: the search hit a drawer was opened from, and
// prefetched `GET /torrents/{hash}` bodies (an LRU of 50, each good for a minute).
import type { components } from '$lib/api/schema';

export type TorrentDetail = components['schemas']['TorrentDetail'];
export type SearchHit = components['schemas']['SearchHit'];

const LIMIT = 50;
export const FRESH_MS = 60_000;

const hits = new Map<string, SearchHit>();
const details = new Map<string, { at: number; detail: TorrentDetail }>();
let generation = 0;
const invalidations = new Set<(revalidate: boolean) => void>();

/** Capture before a request; check again before publishing or caching its response. */
export const detailGeneration = () => generation;
export function onDetailInvalidated(fn: (revalidate: boolean) => void) {
	invalidations.add(fn);
	return () => void invalidations.delete(fn);
}

export function forgetDetail(hash: string) {
	hits.delete(hash);
	details.delete(hash);
}

function remember<V>(map: Map<string, V>, key: string, value: V) {
	map.delete(key);
	map.set(key, value);
	while (map.size > LIMIT) map.delete(map.keys().next().value as string);
}

export const cacheHit = (hash: string, hit: SearchHit) => remember(hits, hash, hit);
export const cachedHit = (hash: string): SearchHit | undefined => hits.get(hash);

export function cacheDetail(hash: string, detail: TorrentDetail, now = Date.now()) {
	remember(details, hash, { at: now, detail });
}

export function cachedDetail(hash: string, now = Date.now()): TorrentDetail | undefined {
	const entry = details.get(hash);
	if (!entry) return undefined;
	if (now - entry.at > FRESH_MS) {
		details.delete(hash);
		return undefined;
	}
	// Touch for LRU order.
	details.delete(hash);
	details.set(hash, entry);
	return entry.detail;
}

export function clearDetailCaches(revalidate = true) {
	generation++;
	hits.clear();
	details.clear();
	for (const fn of [...invalidations]) fn(revalidate);
}
