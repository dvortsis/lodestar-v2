// "+N other versions", opened in place (UI review 02 §5.1): a grouped row expands to at most five
// compact rows of its group's other versions, then "Show all" opens the best version's drawer,
// where Related lists the rest (GS-4). The extra rows are rows of the list, at its one fixed
// height (§10.1), so j/k and the arrows walk them like any other row.
//
// The versions come from `GET /torrents/{hash}/siblings` (`other_versions`, best first): the same
// title identity the server groups by (docs/decisions/2026-11-16-GS-3.md), confirmed and withheld
// as the server does. The list is the work's versions, not only those matching the query, so the
// in-place rows can differ from `others`; "Show all" counts the server's `others` and the row.
import { api, isAbort } from '$lib/api/client';
import { formatCount } from '$lib/format';
import { currentLocale } from '$lib/i18n/locale.svelte';
import { m } from '$lib/paraglide/messages.js';
import type { SearchHit } from '../results/ResultRow.svelte';

/** Rows shown in place before "Show all" (UI review 02 §5.1). */
export const SHOWN = 5;

export type VersionRow = { row: 'version'; parent: SearchHit; hit: SearchHit; key: string };
export type StateRow = {
	row: 'more' | 'loading' | 'failed' | 'none';
	parent: SearchHit;
	key: string;
};
export type GroupRow = VersionRow | StateRow;
export type ListRow = SearchHit | GroupRow;

export const isGroupRow = (r: ListRow): r is GroupRow => 'row' in r;
/** The torrent a row stands for: a version's own, else its group's best. */
export const hitOf = (r: ListRow): SearchHit =>
	isGroupRow(r) ? (r.row === 'version' ? r.hit : r.parent) : r;
/** A row's list key. A version's carries its group, so it never meets another row's key. */
export const keyOf = (r: ListRow): string => (isGroupRow(r) ? r.key : r.hash);

type Open =
	| { status: 'loading' }
	| { status: 'failed' }
	| { status: 'ready'; versions: SearchHit[]; truncated: boolean };

export interface ExpandOptions {
	/** A group's load ended: what to say politely about it (§10.13), for the list's live region. */
	onsettled?: (text: string) => void;
}

/** What a group's load comes to, said once it lands: how many versions show, or that it failed. */
export function settledText(versions: number | null): string {
	if (versions === null) return m.search_collapse_failed();
	if (versions === 0) return m.search_collapse_none();
	if (versions === 1) return m.search_collapse_shown_one();
	return m.search_collapse_shown_other({ count: formatCount(versions, currentLocale()) });
}

export function createExpand(options: ExpandOptions = {}) {
	// Open groups: a raw state replaced whole on every change, never mutated, so a plain Map.
	// eslint-disable-next-line svelte/prefer-svelte-reactivity
	let open = $state.raw(new Map<string, Open>());
	/** The query the open groups belong to: a new one closes them all. */
	let scope: string | null = null;
	/** Loads in flight: bookkeeping only, never rendered. */
	// eslint-disable-next-line svelte/prefer-svelte-reactivity
	const requests = new Map<string, AbortController>();

	function set(hash: string, value: Open | undefined) {
		// eslint-disable-next-line svelte/prefer-svelte-reactivity
		const next = new Map(open);
		if (value) next.set(hash, value);
		else next.delete(hash);
		open = next;
	}

	async function fetchVersions(hit: SearchHit) {
		requests.get(hit.hash)?.abort();
		const ctl = new AbortController();
		requests.set(hit.hash, ctl);
		set(hit.hash, { status: 'loading' });
		try {
			const r = await api.get('/torrents/{hash}/siblings', {
				path: { hash: hit.hash },
				signal: ctl.signal
			});
			if (requests.get(hit.hash) !== ctl) return;
			const group = r.other_versions;
			const versions = group.hits.filter((h) => h.hash !== hit.hash);
			set(hit.hash, { status: 'ready', versions, truncated: group.truncated });
			options.onsettled?.(settledText(Math.min(versions.length, SHOWN)));
		} catch (e) {
			if (isAbort(e) || requests.get(hit.hash) !== ctl) return;
			set(hit.hash, { status: 'failed' });
			options.onsettled?.(settledText(null));
		} finally {
			if (requests.get(hit.hash) === ctl) requests.delete(hit.hash);
		}
	}

	/** Opens a group with other versions; a failed one is fetched again. */
	function expand(hit: SearchHit) {
		const now = open.get(hit.hash);
		if (!hit.others || (now && now.status !== 'failed')) return;
		void fetchVersions(hit);
	}

	function close(hash: string) {
		requests.get(hash)?.abort();
		requests.delete(hash);
		set(hash, undefined);
	}

	return {
		/** Follows the rows' query (`results.shown`): a new query starts with every group closed. */
		follow(shown: string | null) {
			if (shown === scope) return;
			scope = shown;
			for (const ctl of requests.values()) ctl.abort();
			requests.clear();
			// eslint-disable-next-line svelte/prefer-svelte-reactivity
			if (open.size) open = new Map();
		},
		isOpen: (hash: string) => open.has(hash),
		expand,
		close,
		toggle(hit: SearchHit) {
			if (open.has(hit.hash)) close(hit.hash);
			else expand(hit);
		},
		/** The list's rows: each open group's versions (or its state) right under its best row. */
		rows(hits: SearchHit[]): ListRow[] {
			if (open.size === 0) return hits;
			const out: ListRow[] = [];
			for (const hit of hits) {
				out.push(hit);
				const g = open.get(hit.hash);
				if (!g) continue;
				const k = `group:${hit.hash}`;
				if (g.status !== 'ready') {
					out.push({ row: g.status, parent: hit, key: `${k}:${g.status}` });
					continue;
				}
				if (g.versions.length === 0) {
					out.push({ row: 'none', parent: hit, key: `${k}:none` });
					continue;
				}
				for (const v of g.versions.slice(0, SHOWN))
					out.push({ row: 'version', parent: hit, hit: v, key: `${k}:${v.hash}` });
				if ((hit.others ?? 0) > SHOWN || g.versions.length > SHOWN || g.truncated)
					out.push({ row: 'more', parent: hit, key: `${k}:more` });
			}
			return out;
		},
		/**
		 * → opens the focused row's group, ← closes it (from the best row or one of its versions).
		 * Returns the hash of the row to focus when focus must move (to the best row, after ← on a
		 * version), else undefined. Handled keys are prevented.
		 */
		key(event: KeyboardEvent, row: ListRow): string | undefined {
			if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
			const parent = isGroupRow(row) ? row.parent : row;
			if (event.key === 'ArrowRight' && !isGroupRow(row) && row.others) {
				event.preventDefault();
				expand(row);
			} else if (event.key === 'ArrowLeft' && open.has(parent.hash)) {
				event.preventDefault();
				close(parent.hash);
				if (isGroupRow(row)) return parent.hash;
			}
			return undefined;
		}
	};
}

export type Expand = ReturnType<typeof createExpand>;
