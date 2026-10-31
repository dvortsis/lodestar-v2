// Zero-result recovery and index-state banners (B-search-recovery): pure mappings from the server's
// `recovery` and `/search/state` to what the panel shows. The server decides every number, flag and
// reduced query; nothing here re-parses or re-counts.
import type { components } from '$lib/api/schema';

type Recovery = components['schemas']['SearchRecovery'];
export type IndexState = components['schemas']['SearchIndexState'];
type Rebuild = Omit<components['schemas']['IndexRebuild'], 'started_at'>;

export interface Suggestion {
	clause: string;
	total: number;
	approx: boolean;
	without: components['schemas']['AstNode'];
}

/** "Removing X gives N": only clauses with a count above 0 (absent means the deadline passed). */
export function suggestions(recovery: Recovery | undefined | null): Suggestion[] {
	return (recovery?.clauses ?? []).flatMap((c) =>
		c.total !== undefined && c.total > 0
			? [{ clause: c.clause, total: c.total, approx: c.total_approx, without: c.without }]
			: []
	);
}

export type Banner =
	| { kind: 'rebuild'; rebuild: Rebuild }
	| { kind: 'growing'; count: number; perHour: number | null }
	| null;

/** A rebuild in progress (done and failed are over) beats the growing banner. */
export function bannerFor(state: IndexState | null, live: Rebuild | null | undefined): Banner {
	if (!state) return null;
	const rebuild = live ?? state.rebuild;
	if (rebuild && rebuild.phase !== 'done' && rebuild.phase !== 'failed')
		return { kind: 'rebuild', rebuild };
	if (state.growing)
		return {
			kind: 'growing',
			count: state.searchable_torrents,
			perHour:
				state.growth_per_hour != null && state.growth_per_hour > 0 ? state.growth_per_hour : null
		};
	return null;
}
