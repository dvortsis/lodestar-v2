// The plain sentence at the top of Insights (UI review 2026-11-13, U25): is it working, what did it
// add, does anything need me. Every clause is chosen from a response field; it never says "working"
// unless the data says so, and a state with no data (loading, error, stale, starting) says that.
import { formatCount } from '$lib/format';
import { m } from '$lib/paraglide/messages.js';
import type { Overview, Startup } from './model';

export type HealthKind =
	| 'loading'
	| 'error'
	| 'stale'
	| 'paused'
	| 'starting'
	| 'unknown'
	| 'stalled'
	| 'degraded'
	| 'working';

export interface HealthInput {
	phase: 'loading' | 'ready' | 'error';
	overview: Overview | undefined;
	startup: Startup;
	/** Whether the all-time startup check is still open or failed (`startup` is `'no'` then). */
	startupCheck?: 'pending' | 'failed';
	/** The selected range is "all": the sentence says "so far", not "over all time". */
	allTime?: boolean;
	/** The no-progress verdict (`Overview.svelte`'s `stalled`). */
	stalled: boolean;
	/** The range as a phrase ("the last 7 days"). */
	rangeText: string;
	locale: string;
}

export function healthKind({
	phase,
	overview,
	startup,
	startupCheck,
	stalled
}: HealthInput): HealthKind {
	if (phase === 'error') return 'error';
	if (!overview) return 'loading';
	if (overview.stale) return 'stale';
	if (overview.kpi.health.state === 'paused') return 'paused';
	if (startup === 'yes') return 'starting';
	// Whether it has only just started is not known yet: say nothing about "working".
	if (startupCheck === 'pending') return 'loading';
	if (stalled) return 'stalled';
	if (overview.kpi.health.state === 'degraded') return 'degraded';
	if (startupCheck === 'failed' && overview.kpi.new_searchable === 0) return 'unknown';
	return 'working';
}

function attention(count: number, locale: string): string {
	if (count === 0) return m.insights_health_attention_none();
	const n = formatCount(count, locale);
	return count === 1
		? m.insights_health_attention_one()
		: m.insights_health_attention_many({ count: n });
}

export function healthSentence(input: HealthInput): { kind: HealthKind; text: string } {
	const kind = healthKind(input);
	const { overview, rangeText, locale } = input;
	const count = overview?.exceptions.length ?? 0;
	switch (kind) {
		case 'loading':
			return { kind, text: m.insights_health_loading() };
		case 'error':
			return { kind, text: m.insights_health_error() };
		case 'stale':
			return { kind, text: m.insights_health_stale() };
		case 'paused':
			return { kind, text: m.insights_health_paused() };
		case 'unknown':
			return { kind, text: m.insights_health_unknown({ range: rangeText }) };
		case 'starting':
			return { kind, text: m.insights_health_starting() };
		case 'stalled':
			return { kind, text: `${m.insights_health_stalled()} ${attention(count, locale)}` };
		case 'degraded':
			return { kind, text: `${m.insights_health_degraded()} ${attention(count, locale)}` };
		default: {
			const added = overview?.kpi.new_searchable ?? 0;
			const n = formatCount(added, locale);
			const head = input.allTime
				? added === 1
					? m.insights_health_working_all_one()
					: m.insights_health_working_all({ count: n })
				: added === 1
					? m.insights_health_working_one({ range: rangeText })
					: m.insights_health_working({ range: rangeText, count: n });
			return { kind, text: `${head} ${attention(count, locale)}` };
		}
	}
}
