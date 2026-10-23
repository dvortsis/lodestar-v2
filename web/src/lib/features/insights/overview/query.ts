// The Insights overview's URL contract (§10.3, §10.9.4): `/insights?range=24h|7d|30d|all|custom&from=&to=&compare=1`.
// Defaults (`7d`, no compare) are left out. Parsing never throws: anything invalid falls back.

export const RANGES = ['24h', '7d', '30d', 'all', 'custom'] as const;
export type RangeId = (typeof RANGES)[number];

export interface InsightsQuery {
	range: RangeId;
	/** Unix seconds, `custom` only. */
	from?: number;
	/** Unix seconds, exclusive, `custom` only. */
	to?: number;
	compare: boolean;
}

export const DEFAULT_QUERY: InsightsQuery = { range: '7d', compare: false };

const OWN = ['range', 'from', 'to', 'compare'];
const DAY = 86400;

function seconds(value: string | null): number | undefined {
	return value !== null && /^\d{1,12}$/.test(value) ? Number(value) : undefined;
}

export function parseQuery(params: URLSearchParams): InsightsQuery {
	const compare = params.get('compare') === '1';
	const range = params.get('range');
	if (range === 'custom') {
		const from = seconds(params.get('from'));
		const to = seconds(params.get('to'));
		if (from !== undefined && to !== undefined && from < to) {
			return { range, from, to, compare };
		}
		return { ...DEFAULT_QUERY, compare };
	}
	if (range === '24h' || range === '7d' || range === '30d' || range === 'all') {
		return { range, compare };
	}
	return { ...DEFAULT_QUERY, compare };
}

/** The query string (with `?`, or empty) for `q`, keeping the other parameters (`density`). */
export function searchFor(current: URLSearchParams, q: InsightsQuery): string {
	const next = new URLSearchParams();
	for (const [k, v] of current) if (!OWN.includes(k)) next.append(k, v);
	if (q.range !== DEFAULT_QUERY.range) next.set('range', q.range);
	if (q.range === 'custom' && q.from !== undefined && q.to !== undefined) {
		next.set('from', String(q.from));
		next.set('to', String(q.to));
	}
	if (q.compare) next.set('compare', '1');
	const s = next.toString();
	return s ? `?${s}` : '';
}

/** True when the URL is not in the canonical form of what it parses to (so it is replaced). */
export function needsCanonical(current: URLSearchParams): boolean {
	const own = new URLSearchParams();
	for (const [k, v] of current) if (OWN.includes(k)) own.append(k, v);
	const canonical = new URLSearchParams(searchFor(new URLSearchParams(), parseQuery(current)));
	return own.toString() !== canonical.toString();
}

/** Compare has no meaning for `all` (the server sends no `previous`). */
export function compareAllowed(q: InsightsQuery): boolean {
	return q.range !== 'all';
}

/** Query parameters of `getInsightsOverview`. */
export function apiQuery(q: InsightsQuery) {
	return {
		range: q.range,
		from: q.range === 'custom' ? q.from : undefined,
		to: q.range === 'custom' ? q.to : undefined,
		compare: q.compare && compareAllowed(q) ? 1 : undefined
	};
}

/** `YYYY-MM-DD` (a UTC day) → Unix seconds at its start; undefined if not a real date. */
export function dayStart(day: string): number | undefined {
	if (!/^\d{4}-\d{2}-\d{2}$/.test(day)) return undefined;
	const ms = Date.parse(`${day}T00:00:00Z`);
	return Number.isNaN(ms) || new Date(ms).toISOString().slice(0, 10) !== day
		? undefined
		: ms / 1000;
}

/** The UTC day of Unix seconds, `YYYY-MM-DD`. */
export function dayOf(unix: number): string {
	return new Date(unix * 1000).toISOString().slice(0, 10);
}

/** Two chosen UTC days (both included) → `from`/`to`. Undefined unless both are real and ordered. */
export function customFromDays(
	fromDay: string,
	toDay: string
): { from: number; to: number } | undefined {
	const from = dayStart(fromDay);
	const last = dayStart(toDay);
	if (from === undefined || last === undefined || last < from) return undefined;
	return { from, to: last + DAY };
}

/** The last included UTC day of a custom range (`to` is exclusive). */
export function lastDay(to: number): string {
	return dayOf(to - 1);
}
