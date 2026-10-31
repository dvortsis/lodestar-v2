// Zero-result recovery and index-state banners (B-search-recovery): pure mappings from the server's
// `recovery` and `/search/state` to what the panel shows. The server decides every number, flag and
// reduced query; nothing here re-parses or re-counts.
// (The counted-clause mapping is in suggestions.ts so only the Recover chunk carries it.)
import type { components } from '$lib/api/schema';

export type IndexState = components['schemas']['SearchIndexState'];
type Rebuild = Omit<components['schemas']['IndexRebuild'], 'started_at'>;

export type Banner = { kind: 'rebuild'; rebuild: Rebuild } | { kind: 'growing' } | null;

/** A rebuild in progress (done and failed are over) beats the growing banner. */
export function bannerFor(state: IndexState | null, live: Rebuild | null | undefined): Banner {
	const rebuild = live ?? state?.rebuild;
	if (rebuild && rebuild.phase !== 'done' && rebuild.phase !== 'failed')
		return { kind: 'rebuild', rebuild };
	return state?.growing ? { kind: 'growing' } : null;
}
