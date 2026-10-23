// View-model helpers over `OverviewResponse`. They choose, order and name; they never compute a
// number to show (metrics.md §1: every figure is a response field through a formatter).
import type { components } from '$lib/api/schema';
import type { Stage } from './links';
import type { RangeId } from './query';

export type Overview = components['schemas']['OverviewResponse'];
export type Exception = components['schemas']['InsightsException'];
export type FunnelTotal = components['schemas']['FunnelTotal'];

/** Errors before warnings, then the longest-standing first; the sort is stable. */
export function sortExceptions(list: readonly Exception[]): Exception[] {
	const rank = (e: Exception) => (e.severity === 'error' ? 0 : 1);
	return [...list].sort((a, b) => rank(a) - rank(b) || a.since - b.since);
}

/** The level each stage in the strip scrolls to (record: docs/decisions/2026-10-23-S-insights-1.md). */
export const STRIP_TARGET: Record<Stage, 'movement' | 'funnel' | 'segments' | 'status' | 'serve'> =
	{
		discover: 'movement',
		fetch: 'funnel',
		understand: 'segments',
		index: 'status',
		serve: 'serve'
	};

export type Startup = 'no' | 'check' | 'yes';

/**
 * "Fewer than one completed cohort" is install-wide, but `final_cohorts` belongs to the range, so a
 * range with none asks once more over `all`. `check` = that request is still to be made.
 */
export function startupState(
	range: RangeId,
	current: Pick<Overview, 'funnel_total'>,
	all: Pick<Overview, 'funnel_total'> | undefined
): Startup {
	if (current.funnel_total.final_cohorts > 0) return 'no';
	if (range === 'all') return 'yes';
	if (!all) return 'check';
	return all.funnel_total.final_cohorts > 0 ? 'no' : 'yes';
}

export const FUNNEL_STAGES = [
	'harvested',
	'peer_found',
	'fetched',
	'stored',
	'searchable'
] as const;
export type FunnelStage = (typeof FUNNEL_STAGES)[number];

/** The stage page a funnel stage drills into. */
export const FUNNEL_PAGE: Record<FunnelStage, Stage> = {
	harvested: 'discover',
	peer_found: 'discover',
	fetched: 'fetch',
	stored: 'understand',
	searchable: 'index'
};

/** Server-computed conversion into a stage; the first stage has none. */
export function conversionInto(
	total: Pick<FunnelTotal, 'conversion'>,
	stage: FunnelStage
): number | null {
	return stage === 'harvested' ? null : total.conversion[stage];
}

export interface LossRow {
	reason: string;
	count: number;
	share: number | null;
}

/** Reasons that lost anything, largest first (ties by name, so the order is stable). */
export function lossRows(total: Pick<FunnelTotal, 'losses' | 'loss_share'>): LossRow[] {
	return Object.entries(total.losses)
		.filter(([, count]) => count > 0)
		.map(([reason, count]) => ({ reason, count, share: total.loss_share[reason] ?? null }))
		.sort((a, b) => b.count - a.count || a.reason.localeCompare(b.reason));
}
