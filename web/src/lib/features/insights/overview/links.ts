// Drill-through URLs from the overview (§10.9.4, §10.17). Pure, so each can be tested against the
// range it is given.
import { dayOf, lastDay, type InsightsQuery } from './query';

export type Stage = 'discover' | 'fetch' | 'understand' | 'index' | 'serve';
export const STAGES: readonly Stage[] = ['discover', 'fetch', 'understand', 'index', 'serve'];

function rangeParams(q: InsightsQuery, compare: boolean): URLSearchParams {
	const p = new URLSearchParams({ range: q.range });
	if (q.range === 'custom' && q.from !== undefined && q.to !== undefined) {
		p.set('from', String(q.from));
		p.set('to', String(q.to));
	}
	if (compare && q.compare && q.range !== 'all') p.set('compare', '1');
	return p;
}

/** A stage page with the range kept (Detail links also keep `compare`; drill-throughs don't). */
export function stageHref(
	stage: Stage,
	q: InsightsQuery,
	opts: { compare?: boolean; reason?: string } = {}
): string {
	const p = rangeParams(q, opts.compare === true);
	if (opts.reason) p.set('reason', opts.reason);
	return `/insights/${stage}?${p}`;
}

/** Content types in each composition group (docs/decisions/2026-10-23-B11a.md). */
export const GROUP_TYPES: Record<string, readonly string[]> = {
	video: ['movie', 'tv', 'video'],
	audio: ['music', 'audiobook'],
	documents: ['ebook', 'comic'],
	software: ['software', 'game'],
	images: ['images'],
	other: ['other'],
	adult: ['adult']
};

/** The `discovered:` clause for the range, or none for `all`. */
export function discoveredClause(q: InsightsQuery): string | undefined {
	if (q.range === '24h') return 'discovered:<24h';
	if (q.range === '7d') return 'discovered:<7d';
	if (q.range === '30d') return 'discovered:<30d';
	if (q.range === 'custom' && q.from !== undefined && q.to !== undefined) {
		return `discovered:${dayOf(q.from)}..${lastDay(q.to)}`;
	}
	return undefined;
}

/**
 * A Search link for a segment bar. A group links to its content types, a type to itself;
 * `unclassified` has no link. Segments count by event time and `discovered:` filters by discovery,
 * so the Search total need not match the bar (§10.17).
 */
export function searchHref(
	kind: 'group' | 'type',
	key: string,
	q: InsightsQuery
): string | undefined {
	const types = kind === 'group' ? GROUP_TYPES[key] : /^[a-z]{1,32}$/.test(key) ? [key] : undefined;
	if (!types || key === 'unclassified') return undefined;
	const clause = discoveredClause(q);
	const text = [`type:${types.join(',')}`, clause].filter(Boolean).join(' ');
	return `/search?q=${encodeURIComponent(text)}`;
}

const TARGET = /^[a-z_]{1,32}$/;

/** An exception's fix link. Only well-formed targets are linked; anything else gets no link. */
export function fixHref(fix: { kind: string; target: string }): string | undefined {
	if (!TARGET.test(fix.target)) return undefined;
	if (fix.kind === 'stage') return `/insights/${fix.target}`;
	if (fix.kind === 'settings') return `/settings/${fix.target}`;
	return undefined;
}

/** CSV export of one overview panel (`GET /insights/export.csv`). */
export function csvHref(panel: string, q: InsightsQuery): string {
	const p = rangeParams(q, true);
	return `/api/v1/insights/export.csv?panel=${encodeURIComponent(panel)}&${p}`;
}
