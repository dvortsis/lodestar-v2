// Per-worker breakdowns and the Marginal gain view (§19.7). Pure: the response's `by_worker` in,
// formatted rows out. Insights never names a worker itself: names come from `/workers` (admin
// only) and anything missing falls back to "This server" (0) or "Worker <id>".
import type { components } from '$lib/api/schema';
import { formatCount, formatPercent } from '$lib/format';
import { m } from '$lib/paraglide/messages.js';

export type DiscoverWorker = components['schemas']['DiscoverWorker'];
export type FetchWorker = components['schemas']['FetchWorker'];
export interface Row {
	[key: string]: string;
}
export interface Col {
	key: string;
	label: string;
	numeric?: boolean;
}

export function workerName(id: number, names: ReadonlyMap<number, string>): string {
	if (id === 0) return m.insights_workers_this_server();
	return names.get(id) || m.insights_workers_worker({ id: String(id) });
}

export const discoverCols = (): Col[] => [
	{ key: 'worker', label: m.insights_workers_col_worker() },
	{ key: 'granted', label: m.insights_workers_col_granted(), numeric: true },
	{ key: 'marginal', label: m.insights_workers_marginal_title(), numeric: true },
	{ key: 'rediscovered', label: m.insights_workers_col_rediscovered(), numeric: true },
	{ key: 'dropped', label: m.insights_workers_col_dropped(), numeric: true }
];
export const fetchCols = (): Col[] => [
	{ key: 'worker', label: m.insights_workers_col_worker() },
	{ key: 'committed', label: m.insights_workers_col_committed(), numeric: true },
	{ key: 'lost', label: m.insights_workers_col_lost(), numeric: true }
];
export const marginalCols = (): Col[] => [
	{ key: 'worker', label: m.insights_workers_col_worker() },
	{ key: 'marginal', label: m.insights_workers_marginal_title(), numeric: true },
	{ key: 'share', label: m.insights_workers_col_share(), numeric: true }
];

type Names = ReadonlyMap<number, string>;

export function discoverRows(list: DiscoverWorker[], names: Names, locale: string): Row[] {
	return list.map((w) => ({
		worker: workerName(w.worker, names),
		granted: formatCount(w.granted, locale),
		marginal: formatCount(w.marginal, locale),
		rediscovered: formatCount(w.rediscovered, locale),
		dropped: formatCount(w.dropped, locale)
	}));
}
export function fetchRows(list: FetchWorker[], names: Names, locale: string): Row[] {
	return list.map((w) => ({
		worker: workerName(w.worker, names),
		committed: formatCount(w.committed, locale),
		lost: formatCount(w.lost, locale)
	}));
}
/** Marginal as a count and as a share of the worker's own `granted` ("—" when it was granted none). */
export function marginalRows(list: DiscoverWorker[], names: Names, locale: string): Row[] {
	return list.map((w) => ({
		worker: workerName(w.worker, names),
		marginal: formatCount(w.marginal, locale),
		share: w.granted > 0 ? formatPercent(Math.min(1, w.marginal / w.granted), locale, 0) : '—'
	}));
}
