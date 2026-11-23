// The last "still growing" answer this browser saw (X-index-banner-late): when /search/state is slow,
// the rows paint with this decision so the banner above them is already in place. A per-viewer
// convenience only: never trusted for anything but the banner's first frame, and re-read each visit.
import type { IndexState } from './recover';

/** What the growing banner needs: whether it shows, and the figures on its line. */
export type Remembered = Pick<IndexState, 'growing' | 'searchable_torrents' | 'growth_per_hour'>;

const KEY = 'lodestar.indexState';
/** A later format change bumps this, so an old entry reads as nothing remembered. */
const VERSION = 1;

const count = (v: unknown) => typeof v === 'number' && v >= 0 && v < Infinity;
const whole = (v: unknown) => Number.isSafeInteger(v) && (v as number) >= 0;

/** The remembered answer; null when there's none, it's malformed or storage can't be read. */
export function recall(): Remembered | null {
	try {
		const v = JSON.parse(localStorage.getItem(KEY) ?? 'null') as Record<string, unknown> | null;
		const rate = v?.growth_per_hour;
		return v?.v === VERSION &&
			typeof v.growing === 'boolean' &&
			whole(v.searchable_torrents) &&
			(rate === null || count(rate))
			? (v as Remembered)
			: null;
	} catch {
		return null;
	}
}

/** Keeps the live answer for the next visit; a storage that refuses it is ignored. */
export function remember({ growing, searchable_torrents, growth_per_hour }: Remembered) {
	try {
		localStorage.setItem(
			KEY,
			JSON.stringify({ v: VERSION, growing, searchable_torrents, growth_per_hour })
		);
	} catch {
		// private mode, a full quota or blocked site data: the next visit just waits as before
	}
}
