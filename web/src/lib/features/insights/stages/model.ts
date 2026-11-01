// The stage pages' data mapping (§10.9.5). Pure: a response in, labelled metrics out. The client
// computes nothing: a number is a response field through its formatter, and a field the server
// sends as null is "not available yet", never 0 (docs/metrics.md §7.7).
import type { components } from '$lib/api/schema';
import {
	formatBytes,
	formatCount,
	formatLatency,
	formatPercent,
	formatScore,
	formatSpan,
	type MetricValue
} from '$lib/format';
import { m } from '$lib/paraglide/messages.js';

export type StageId = 'discover' | 'fetch' | 'understand' | 'index' | 'serve';
export const STAGE_IDS: readonly StageId[] = ['discover', 'fetch', 'understand', 'index', 'serve'];

export type FetchResponse = components['schemas']['FetchResponse'];
export type UnderstandResponse = components['schemas']['UnderstandResponse'];
export type IndexResponse = components['schemas']['IndexResponse'];
export type ServeResponse = components['schemas']['ServeResponse'];
export type StageResponse =
	DiscoverResponse | FetchResponse | UnderstandResponse | IndexResponse | ServeResponse;
type Bin = components['schemas']['InsightsHistogramBin'];
export type DiscoverResponse = components['schemas']['InsightsDiscoverResponse'];

export type Fmt = 'count' | 'percent' | 'span' | 'latency' | 'bytes' | 'days';

/** One metric: its formatter, and whether the value is an estimate (shown with "≈"). */
export interface Metric {
	key: string;
	label: string;
	value: number | null;
	previous?: number | null;
	fmt: Fmt;
	approx?: boolean;
	/** A reading as of the last update (a queue, a size, a total), not a count over the range. */
	snapshot?: boolean;
}

export interface Series {
	key: string;
	label: string;
	values: (number | null)[];
	previous?: (number | null)[];
	approx?: boolean;
}

export interface ChartSpec {
	id: string;
	title: string;
	series: Series[];
	/** The export panel, when the stage has one. */
	csv?: string;
	/** Every value is an estimate (shown with "≈"). */
	approx?: boolean;
	/** A series of readings at the end of each bucket, not counts per bucket: no rate unit. */
	snapshot?: boolean;
	/** The axis unit of a snapshot chart; torrents when absent. */
	snapshotUnit?: string;
}

export interface Table {
	id: string;
	title: string;
	columns: { key: string; label: string; numeric?: boolean }[];
	rows: Record<string, string>[];
	csv?: string;
	/** Why there are no rows, when the server sent none for a reason worth saying. */
	empty?: string;
}

export interface BarsSpec {
	id: string;
	title: string;
	items: { key: string; label: string; value: number; text: string; sub?: string }[];
	/** The server sent no list (null): not available, which is not an empty list. */
	unavailable?: boolean;
	csv?: string;
}

export interface HistSpec {
	id: string;
	title: string;
	/** `null` when the server has no data yet. */
	bins: Bin[] | null;
	fmt: 'score' | 'percent';
}

export interface StageModel {
	kpis: Metric[];
	more: Metric[];
	charts: ChartSpec[];
	bars: BarsSpec[];
	hists: HistSpec[];
	tables: Table[];
	/** Short facts that are text, not numbers (a rebuild status). */
	notes: { key: string; label: string; text: string }[];
}

// Snapshot metrics (docs/metrics.md): a state now, not events in the range.
const SNAPSHOT = new Set([
	'queue_depth',
	'in_flight',
	'queue_age',
	'dht_nodes',
	'reply_ratio',
	'keyspace',
	'seen_filter',
	'searchable',
	'lag',
	'projection',
	'data',
	'disk_free',
	'db',
	'index',
	'files',
	'segments',
	'merges',
	'file_cov',
	'scrape_cov',
	'scrape_fresh'
]);

const metric = (
	key: string,
	label: string,
	value: number | null,
	fmt: Fmt,
	previous?: number | null,
	approx = false
): Metric => ({ key, label, value, fmt, previous, approx, snapshot: SNAPSHOT.has(key) });

/** One value through its formatter. Latency percentiles and `approx` metrics always carry "≈". */
export function formatMetric(x: Metric, v: number | null | undefined, locale?: string): string {
	const approx = x.approx || x.fmt === 'latency';
	const value: MetricValue =
		v === null || v === undefined ? null : approx ? { value: v, approx } : v;
	switch (x.fmt) {
		case 'count':
			return formatCount(value, locale);
		case 'percent':
			return formatPercent(value, locale, 1);
		case 'span':
			return formatSpan(value, locale);
		case 'latency':
			return formatLatency(value, locale);
		case 'bytes':
			return formatBytes(value, locale);
		case 'days':
			// The server counts days; `formatSpan` takes seconds (a unit change, as metrics.md §5).
			return formatSpan(v === null || v === undefined ? null : v * 86400, locale);
	}
}

const empty = (): StageModel => ({
	kpis: [],
	more: [],
	charts: [],
	bars: [],
	hists: [],
	tables: [],
	notes: []
});

type Pair<T> = { kpi: T; trend: { at: number }[] } | undefined;
const prevOf = <K>(p: { kpi: K } | undefined, f: (k: K) => number | null) =>
	p ? f(p.kpi) : undefined;

/** A series with the previous period's values only when the lengths agree (else dropped). */
function series<P extends { at: number }>(
	trend: P[],
	previous: Pair<unknown> | undefined,
	key: string,
	label: string,
	pick: (p: P) => number | null,
	approx = false
): Series {
	const prev = previous?.trend as P[] | undefined;
	return {
		key,
		label,
		values: trend.map(pick),
		previous: prev && prev.length === trend.length ? prev.map(pick) : undefined,
		approx
	};
}

export function mapDiscover(r: DiscoverResponse): StageModel {
	const k = r.kpi;
	const p = r.previous;
	const out = empty();
	// Harvest uniqueness comes from the rotating seen filter, so "unique" is always approximate
	// (§10.9.3).
	out.kpis = [
		metric(
			'harvested',
			m.insights_stage_discover_harvested(),
			k.harvested,
			'count',
			prevOf(p, (x) => x.harvested),
			true
		),
		metric(
			'reported',
			m.insights_stage_discover_reported(),
			k.reported,
			'count',
			prevOf(p, (x) => x.reported)
		),
		metric('dht_nodes', m.insights_stage_discover_nodes(), k.dht_nodes, 'count'),
		metric('reply_ratio', m.insights_stage_discover_reply(), k.reply_ratio, 'percent')
	];
	out.more = [
		metric(
			'rediscovered',
			m.insights_stage_discover_rediscovered(),
			k.rediscovered,
			'count',
			prevOf(p, (x) => x.rediscovered)
		),
		metric(
			'dropped',
			m.insights_stage_discover_dropped(),
			k.dropped,
			'count',
			prevOf(p, (x) => x.dropped)
		),
		metric('bep51', m.insights_stage_discover_bep51(), k.bep51_sample_yield, 'percent'),
		metric('keyspace', m.insights_stage_discover_keyspace(), k.keyspace_coverage, 'percent'),
		metric('seen_filter', m.insights_stage_discover_seen_filter(), k.seen_filter_load, 'percent')
	];
	out.charts = [
		{
			id: 'discover-harvested',
			title: m.insights_stage_discover_chart_harvested(),
			csv: 'discover.trend',
			approx: true,
			series: [
				series(
					r.trend,
					p,
					'harvested',
					m.insights_stage_discover_harvested(),
					(x) => x.harvested,
					true
				)
			]
		},
		{
			id: 'discover-reported',
			title: m.insights_stage_discover_chart_reported(),
			series: [
				series(r.trend, p, 'reported', m.insights_stage_discover_reported(), (x) => x.reported),
				series(
					r.trend,
					p,
					'rediscovered',
					m.insights_stage_discover_rediscovered(),
					(x) => x.rediscovered
				),
				series(r.trend, p, 'dropped', m.insights_stage_discover_dropped(), (x) => x.dropped)
			]
		}
	];
	return out;
}

const STRATEGIES: Record<string, () => string> = {
	sample: () => m.insights_stage_discover_strategy_sample(),
	get_peers: () => m.insights_stage_discover_strategy_get_peers(),
	announce: () => m.insights_stage_discover_strategy_announce(),
	lookup: () => m.insights_stage_discover_strategy_lookup(),
	refetch: () => m.insights_stage_discover_strategy_refetch()
};

export function discoverBars(r: DiscoverResponse, locale?: string): BarsSpec[] {
	return [
		{
			id: 'by-strategy',
			title: m.insights_stage_discover_by_strategy(),
			csv: 'discover.by_strategy',
			items: r.by_strategy.map((x) => ({
				key: x.key,
				label: STRATEGIES[x.key]?.() ?? x.key,
				value: x.reported,
				text: formatCount(x.reported, locale)
			}))
		}
	];
}

export function discoverTables(r: DiscoverResponse, locale?: string): Table[] {
	return [
		{
			id: 'by-source',
			title: m.insights_stage_discover_by_source(),
			csv: 'discover.by_source',
			columns: [
				{ key: 'key', label: m.insights_stage_discover_col_source() },
				{ key: 'reported', label: m.insights_stage_discover_reported(), numeric: true },
				{ key: 'rediscovered', label: m.insights_stage_discover_rediscovered(), numeric: true },
				{ key: 'dropped', label: m.insights_stage_discover_dropped(), numeric: true }
			],
			rows: r.by_source.map((x) => ({
				key: x.key,
				reported: formatCount(x.reported, locale),
				rediscovered: formatCount(x.rediscovered, locale),
				dropped: formatCount(x.dropped, locale)
			}))
		},
		{
			id: 'by-ip-family',
			title: m.insights_stage_discover_by_ip(),
			columns: [
				{ key: 'key', label: m.insights_stage_discover_col_family() },
				{ key: 'harvested', label: m.insights_stage_discover_harvested(), numeric: true },
				{ key: 'searchable', label: m.insights_stage_index_searchable(), numeric: true },
				{ key: 'yield', label: m.insights_stage_discover_col_yield(), numeric: true }
			],
			rows: (r.by_ip_family ?? []).map((x) => ({
				key: x.key,
				harvested: formatCount({ value: x.harvested, approx: true }, locale),
				searchable: formatCount(x.searchable, locale),
				yield: formatPercent(x.yield, locale, 1)
			})),
			empty: r.by_ip_family === null ? m.insights_stage_na() : undefined
		}
	];
}

export function mapFetch(r: FetchResponse): StageModel {
	const k = r.kpi;
	const p = r.previous;
	const pk = (f: (x: typeof k) => number | null) => prevOf(p, f);
	const out = empty();
	out.kpis = [
		metric(
			'fetched',
			m.insights_stage_fetch_fetched(),
			k.fetched,
			'count',
			pk((x) => x.fetched)
		),
		metric(
			'lost',
			m.insights_stage_fetch_lost(),
			k.lost,
			'count',
			pk((x) => x.lost)
		),
		metric('queue_depth', m.insights_stage_fetch_queue_depth(), k.queue_depth, 'count'),
		metric('latency_p95', m.insights_stage_fetch_latency_p95(), k.fetch_latency_p95_ms, 'latency')
	];
	out.more = [
		metric('in_flight', m.insights_stage_fetch_in_flight(), k.in_flight, 'count'),
		metric('queue_age', m.insights_stage_fetch_queue_age(), k.queue_age_s, 'span'),
		metric('latency_p50', m.insights_stage_fetch_latency_p50(), k.fetch_latency_p50_ms, 'latency'),
		metric('utp_share', m.insights_stage_fetch_utp_share(), k.utp_share, 'percent'),
		metric('mse_share', m.insights_stage_fetch_mse_share(), k.mse_share, 'percent'),
		metric(
			'limiter',
			m.insights_stage_fetch_limiter(),
			k.limiter_rejections,
			'count',
			pk((x) => x.limiter_rejections)
		),
		metric('ports', m.insights_stage_fetch_ports(), k.ephemeral_port_pressure, 'percent')
	];
	out.charts = [
		{
			id: 'fetch-trend',
			title: m.insights_stage_fetch_chart(),
			csv: 'fetch.trend',
			series: [
				series(r.trend, p, 'fetched', m.insights_stage_fetch_fetched(), (x) => x.fetched),
				series(r.trend, p, 'lost', m.insights_stage_fetch_lost(), (x) => x.lost)
			]
		}
	];
	return out;
}

export function mapUnderstand(r: UnderstandResponse): StageModel {
	const k = r.kpi;
	const p = r.previous;
	const out = empty();
	out.kpis = [
		metric(
			'stored',
			m.insights_stage_understand_stored(),
			k.stored,
			'count',
			prevOf(p, (x) => x.stored)
		),
		metric(
			'newly',
			m.insights_stage_understand_newly(),
			k.newly_searchable,
			'count',
			prevOf(p, (x) => x.newly_searchable)
		),
		metric(
			'blocked',
			m.insights_stage_understand_blocked(),
			k.blocked,
			'count',
			prevOf(p, (x) => x.blocked)
		),
		metric('resolution', m.insights_stage_understand_resolution(), k.resolution_rate, 'percent')
	];
	out.more = [
		metric('corrections', m.insights_stage_understand_corrections(), k.recent_corrections, 'count')
	];
	out.charts = [
		{
			id: 'understand-trend',
			title: m.insights_stage_understand_chart(),
			csv: 'understand.trend',
			series: [
				series(r.trend, p, 'stored', m.insights_stage_understand_stored(), (x) => x.stored),
				series(r.trend, p, 'blocked', m.insights_stage_understand_blocked(), (x) => x.blocked)
			]
		}
	];
	out.hists = [
		{
			id: 'confidence',
			title: m.insights_stage_understand_confidence(),
			bins: k.parser_confidence,
			fmt: 'percent'
		},
		{
			id: 'junk',
			title: m.insights_stage_understand_junk(),
			bins: k.junk_distribution,
			fmt: 'score'
		}
	];
	return out;
}

export function mapIndex(r: IndexResponse): StageModel {
	const k = r.kpi;
	const p = r.previous;
	const out = empty();
	out.kpis = [
		metric(
			'searchable',
			m.insights_stage_index_searchable(),
			k.searchable,
			'count',
			prevOf(p, (x) => x.searchable)
		),
		metric(
			'new',
			m.insights_stage_index_new(),
			k.new_searchable,
			'count',
			prevOf(p, (x) => x.new_searchable)
		),
		...(k.new_searchable_imported > 0
			? [
					metric(
						'new_imported',
						m.insights_import_imported(),
						k.new_searchable_imported,
						'count',
						prevOf(p, (x) => x.new_searchable_imported)
					)
				]
			: []),
		metric('lag', m.insights_stage_index_lag(), k.index_lag_s, 'span'),
		metric('projection', m.insights_stage_index_projection(), k.projection_days, 'days')
	];
	out.more = [
		metric(
			'removed',
			m.insights_stage_index_removed(),
			k.removed,
			'count',
			prevOf(p, (x) => x.removed)
		),
		metric('data', m.insights_stage_index_data(), k.data_bytes, 'bytes'),
		metric('disk_free', m.insights_stage_index_disk_free(), k.disk_free_bytes, 'bytes'),
		metric('db', m.insights_stage_index_db(), k.sizes?.database_bytes ?? null, 'bytes'),
		metric('index', m.insights_stage_index_index(), k.sizes?.index_bytes ?? null, 'bytes'),
		metric('files', m.insights_stage_index_files(), k.sizes?.file_index_bytes ?? null, 'bytes'),
		metric('segments', m.insights_stage_index_segments(), k.segments, 'count'),
		metric('merges', m.insights_stage_index_merges(), k.merges, 'count'),
		metric('file_cov', m.insights_stage_index_file_cov(), k.file_index_coverage, 'percent'),
		metric('scrape_cov', m.insights_stage_index_scrape_cov(), k.scrape_coverage, 'percent'),
		metric('scrape_fresh', m.insights_stage_index_scrape_fresh(), k.scrape_freshness_s, 'span')
	];
	if (k.rebuild) {
		const names: Record<string, () => string> = {
			building: () => m.insights_stage_index_rebuild_building(),
			finishing: () => m.insights_stage_index_rebuild_finishing(),
			swapping: () => m.insights_stage_index_rebuild_swapping(),
			failed: () => m.insights_stage_index_rebuild_failed()
		};
		out.notes = [
			{
				key: 'rebuild',
				label: m.insights_stage_index_rebuild(),
				text: names[k.rebuild]?.() ?? k.rebuild
			}
		];
	}
	out.charts = [
		{
			id: 'index-trend',
			title: m.insights_stage_index_chart_new(),
			csv: 'index.trend',
			series: [
				series(r.trend, p, 'new', m.insights_stage_index_new(), (x) => x.new_searchable),
				series(r.trend, p, 'removed', m.insights_stage_index_removed(), (x) => x.removed)
			]
		},
		{
			id: 'index-total',
			snapshot: true,
			title: m.insights_stage_index_chart_total(),
			series: [
				series(r.trend, p, 'searchable', m.insights_stage_index_searchable(), (x) => x.searchable)
			]
		}
	];
	return out;
}

export function mapServe(r: ServeResponse, locale?: string): StageModel {
	const k = r.kpi;
	const p = r.previous;
	const out = empty();
	out.kpis = [
		metric(
			'searches',
			m.insights_stage_serve_searches(),
			k.searches,
			'count',
			prevOf(p, (x) => x.searches)
		),
		metric(
			'zero',
			m.insights_stage_serve_zero(),
			k.zero_result_rate,
			'percent',
			prevOf(p, (x) => x.zero_result_rate)
		),
		metric('p95', m.insights_stage_serve_p95(), k.latency_p95_ms, 'latency'),
		metric('cancel', m.insights_stage_serve_cancel(), k.cancel_rate, 'percent')
	];
	out.more = [
		metric('p50', m.insights_stage_serve_p50(), k.latency_p50_ms, 'latency'),
		// The share of facet counts that were sampled: an estimate by definition.
		metric(
			'facets',
			m.insights_stage_serve_facets(),
			k.facet_sampling_rate,
			'percent',
			undefined,
			true
		),
		metric('alerts', m.insights_stage_serve_alerts(), k.alerts_fired, 'count'),
		metric('delivered', m.insights_stage_serve_delivered(), k.notifications_delivered, 'count'),
		metric('failures', m.insights_stage_serve_failures(), k.notification_failures, 'count'),
		metric('sends', m.insights_stage_serve_sends(), k.sends, 'count')
	];
	out.charts = [
		{
			id: 'serve-trend',
			title: m.insights_stage_serve_chart(),
			csv: 'serve.trend',
			series: [
				series(r.trend, p, 'searches', m.insights_stage_serve_searches(), (x) => x.searches),
				series(r.trend, p, 'zero', m.insights_stage_serve_zero_results(), (x) => x.zero_results)
			]
		}
	];
	const rows = (list: components['schemas']['QueryRow'][] | null, locale?: string) =>
		(list ?? []).map((q) => ({
			query: q.query,
			count: formatCount(q.count, locale),
			zero: formatCount(q.zero, locale)
		}));
	const cols = [
		{ key: 'query', label: m.insights_stage_serve_col_query() },
		{ key: 'count', label: m.insights_stage_serve_col_count(), numeric: true },
		{ key: 'zero', label: m.insights_stage_serve_col_zero(), numeric: true }
	];
	// Query logging is off by default: a null list means no log, and the page says so.
	out.tables = [
		{
			id: 'top-queries',
			title: m.insights_stage_serve_top(),
			columns: cols,
			rows: rows(r.top_queries, locale),
			empty: r.top_queries === null ? m.insights_stage_serve_logging_off() : undefined
		},
		{
			id: 'top-zero',
			title: m.insights_stage_serve_top_zero(),
			columns: cols,
			rows: rows(r.top_zero_result_queries, locale),
			empty: r.top_zero_result_queries === null ? m.insights_stage_serve_logging_off() : undefined
		}
	];
	return out;
}

/** Contiguous bins → edges for the histogram; undefined when they are not contiguous or open-ended. */
export function binEdges(bins: Bin[]): number[] | undefined {
	if (!bins.length) return undefined;
	let at = bins[0]?.lo;
	if (at === undefined) return undefined;
	const edges = [at];
	for (const b of bins) {
		if (b.lo !== at) return undefined;
		at = b.hi;
		edges.push(at);
	}
	return edges;
}

/** Edge labels for the histogram, in the bins' unit. */
export function edgeLabels(edges: number[], fmt: 'score' | 'percent', locale?: string): string[] {
	return edges.map((e) =>
		fmt === 'score' ? formatScore(e, locale) : formatPercent(e / 100, locale, 0)
	);
}

export function binLabel(b: Bin, fmt: 'score' | 'percent', locale?: string): string {
	const f = (n: number) =>
		fmt === 'score' ? formatScore(n, locale) : formatPercent(n / 100, locale, 0);
	return `${f(b.lo)}–${f(b.hi)}`;
}

/** Tables from the stage's segment lists (Understand) and losses (Fetch), by label function. */
export function understandBars(
	r: UnderstandResponse,
	group: (k: string) => string,
	type: (k: string) => string,
	locale?: string
): BarsSpec[] {
	const bars = (list: { key: string; searchable: number }[], label: (k: string) => string) =>
		list.map((s) => ({
			key: s.key,
			label: label(s.key),
			value: s.searchable,
			text: formatCount(s.searchable, locale)
		}));
	return [
		{
			id: 'by-group',
			title: m.insights_stage_understand_by_group(),
			csv: 'understand.by_group',
			items: bars(r.by_group, group)
		},
		{
			id: 'by-type',
			title: m.insights_stage_understand_by_type(),
			csv: 'understand.by_content_type',
			items: bars(r.by_content_type, type)
		}
	];
}

export function fetchBars(
	r: FetchResponse,
	reasonLabel: (k: string) => string,
	locale?: string
): BarsSpec[] {
	return [
		{
			id: 'losses',
			title: m.insights_stage_fetch_losses(),
			csv: 'fetch.losses',
			unavailable: r.losses === null,
			items: (r.losses ?? []).map((l) => ({
				key: l.key,
				label: reasonLabel(l.key),
				value: l.count,
				text: formatCount(l.count, locale),
				sub: formatPercent(l.share, locale, 0)
			}))
		}
	];
}

export function indexBars(
	r: IndexResponse,
	type: (k: string) => string,
	locale?: string
): BarsSpec {
	return {
		id: 'removed',
		title: m.insights_stage_index_removed_by_type(),
		csv: 'index.removed_by_type',
		items: r.removed_by_type.map((x) => ({
			key: x.key,
			label: type(x.key),
			value: x.removed,
			text: formatCount(x.removed, locale)
		}))
	};
}

export function trackerTable(r: IndexResponse, locale?: string): Table {
	return {
		id: 'trackers',
		title: m.insights_stage_index_trackers(),
		columns: [
			{ key: 'key', label: m.insights_stage_index_col_tracker() },
			{ key: 'state', label: m.insights_stage_index_col_state() },
			{ key: 'reporting', label: m.insights_stage_index_col_reporting(), numeric: true }
		],
		rows: (r.trackers ?? []).map((t) => ({
			key: t.key,
			state: t.state,
			reporting: formatCount(t.reporting, locale)
		})),
		empty: r.trackers === null ? m.insights_stage_na() : undefined
	};
}

export function mapStage(stage: StageId, r: StageResponse, locale?: string): StageModel {
	switch (stage) {
		case 'discover':
			return mapDiscover(r as DiscoverResponse);
		case 'fetch':
			return mapFetch(r as FetchResponse);
		case 'understand':
			return mapUnderstand(r as UnderstandResponse);
		case 'index':
			return mapIndex(r as IndexResponse);
		case 'serve':
			return mapServe(r as ServeResponse, locale);
	}
}
