// Related (§10.5 item 5, UI review 02 §5.1): the four lists `GET /torrents/{hash}/siblings` returns,
// and what each row shows. Pure: the section renders what this decides.
import type { components } from '$lib/api/schema';
import type { SearchHit, TorrentDetail } from '../cache';

export type Siblings = components['schemas']['SiblingsResponse'];
export type HitGroup = components['schemas']['HitGroup'];
export type Unavailable = components['schemas']['Unavailable'];

/** The lists in the order the UI plan names them. */
export const LISTS = [
	'other_versions',
	'identical_payload',
	'same_file',
	'similar_payload'
] as const;
export type ListKey = (typeof LISTS)[number];

/** Compare takes 2 to 5 torrents (§10.6); the open torrent is always the first column. */
export const COMPARE_MIN = 2;
export const COMPARE_MAX = 5;

/** A list's "some may be missing" note: the index is still filling it in, or the lookup was bounded. */
export type Gap = 'partial' | 'capped';

export interface List {
	key: ListKey;
	/** Rows to show: never the open torrent itself, never the same torrent twice. */
	hits: SearchHit[];
	/** Why the server can't answer this list; null when it did (possibly with no rows). */
	reason: Unavailable | null;
	/** More matched than the server listed. */
	truncated: boolean;
	gap: Gap | null;
}

/** The four lists, as the server sent them. Nothing the server left out is added back. */
export function lists(s: Siblings, open: string): List[] {
	const gaps: Record<ListKey, Gap | null> = {
		other_versions: null,
		identical_payload: s.identical_payload_partial ? 'partial' : null,
		same_file: s.same_file_partial ? 'partial' : s.same_file_capped ? 'capped' : null,
		similar_payload: s.similar_payload_partial ? 'partial' : null
	};
	return LISTS.map((key) => {
		const g = s[key];
		const unavailable = g.status === 'unavailable';
		const seen = new Set<string>([open]);
		const hits = unavailable
			? []
			: g.hits.filter((h) => (seen.has(h.hash) ? false : (seen.add(h.hash), true)));
		return {
			key,
			hits,
			// An unavailable group without a reason still reads as unavailable (the generic reason).
			reason: unavailable ? (g.reason ?? 'needs_group_minhash') : null,
			truncated: !unavailable && g.truncated,
			gap: unavailable ? null : gaps[key]
		};
	});
}

/** The attributes a row can show, in the order it shows them. */
export const ATTRS = ['title', 'resolution', 'content_type', 'size', 'files', 'seeders'] as const;
export type Attr = (typeof ATTRS)[number];

type Base = { [K in Attr]?: SearchHit[K] };

/**
 * What the open torrent is, for comparison: its search hit when there is one (the parsed fields live
 * there), its detail otherwise. A field the open torrent's data doesn't carry stays unknown, and an
 * unknown field is never called different.
 */
export function base(detail: TorrentDetail, hit: SearchHit | null): Base {
	if (hit) return Object.fromEntries(ATTRS.map((a) => [a, hit[a]])) as Base;
	return { size: detail.size, files: detail.files, seeders: detail.seeders };
}

/** The attributes of `row` that differ from the open torrent's, in `ATTRS` order. */
export function differs(row: SearchHit, open: Base): Attr[] {
	// A missing value and a null one are the same "none".
	return ATTRS.filter((a) => a in open && (open[a] ?? null) !== (row[a] ?? null));
}

/** The /compare columns: the open torrent first, then the picks in the order they were made. */
export function compareSet(open: string, picked: readonly string[]): string[] {
	return [open, ...picked.filter((h) => h !== open)];
}

/** Why "Compare these" can't run yet, or null when it can. */
export function compareBlock(picked: number): 'none' | 'over' | null {
	if (picked + 1 < COMPARE_MIN) return 'none';
	if (picked + 1 > COMPARE_MAX) return 'over';
	return null;
}
