// Hand-built stage responses for the unit and e2e tests. Distinctive values on purpose, so a test
// can tell a field from a number the client made up. Relative imports only (Playwright loads this
// without the `$lib` alias). Never imported by the app.
import type { components } from '../../../api/schema';

type S = components['schemas'];
const NOW = 1_792_756_800;
const HOUR = 3600;
const range: S['InsightsRange'] = { bucket: 'hour', from: NOW - 12 * HOUR, to: NOW, tz: 'UTC' };
const at = (i: number) => NOW - (12 - i) * HOUR;
const pts = <T>(f: (i: number) => T) => Array.from({ length: 12 }, (_, i) => f(i));

export function fetchFixture(over: Partial<S['FetchResponse']> = {}): S['FetchResponse'] {
	return {
		funnel: [],
		funnel_total: {
			cohorts: 0,
			conversion: { fetched: null, peer_found: null, searchable: null, stored: null },
			final_cohorts: 0,
			in_progress: 0,
			loss_share: {},
			losses: {},
			stages: { fetched: 0, harvested: 0, peer_found: 0, searchable: 0, stored: 0 }
		},
		kpi: {
			ephemeral_port_pressure: null,
			fetch_latency_p50_ms: 120,
			fetch_latency_p95_ms: 1400,
			fetched: 4321,
			in_flight: 17,
			limiter_rejections: 9,
			lost: 777,
			mse_share: 0.25,
			queue_age_s: 90,
			queue_depth: 1234,
			utp_share: null
		},
		losses: [
			{ count: 500, key: 'no_peers', share: 0.65 },
			{ count: 277, key: 'timed_out', share: 0.35 }
		],
		range,
		reason: null,
		stale: null,
		trend: pts((i) => ({
			at: at(i),
			fetched: i === 5 ? null : 300 + i,
			lost: i === 5 ? null : 50 + i
		})),
		updated_at: NOW,
		...over
	};
}

export function understandFixture(
	over: Partial<S['UnderstandResponse']> = {}
): S['UnderstandResponse'] {
	return {
		by_content_type: [{ key: 'movie', searchable: 321 }],
		by_group: [{ key: 'video', searchable: 654 }],
		kpi: {
			blocked: 13,
			junk_distribution: null,
			newly_searchable: 2468,
			parser_confidence: [
				{ backfill: false, count: 5, hi: 50, lo: 0 },
				{ backfill: false, count: 95, hi: 100, lo: 50 }
			],
			recent_corrections: null,
			resolution_rate: 0.42,
			stored: 3579
		},
		range,
		stale: null,
		trend: pts((i) => ({ at: at(i), blocked: 1, stored: 100 + i })),
		updated_at: NOW,
		...over
	};
}

export function indexFixture(over: Partial<S['IndexResponse']> = {}): S['IndexResponse'] {
	return {
		kpi: {
			data_bytes: 5_000_000_000,
			disk_free_bytes: null,
			file_index_coverage: null,
			index_lag_s: 4,
			merges: null,
			new_searchable: 1357,
			projection_days: null,
			rebuild: 'building',
			removed: 11,
			scrape_coverage: 0.5,
			scrape_freshness_s: 600,
			searchable: 1_234_567,
			segments: null,
			sizes: null
		},
		range,
		removed_by_type: [{ key: 'movie', removed: 8 }],
		stale: { reason: 'paused', since: NOW - 600 },
		trackers: null,
		trend: pts((i) => ({ at: at(i), new_searchable: 10 + i, removed: 1, searchable: 1000 + i })),
		updated_at: NOW,
		...over
	};
}

export function serveFixture(over: Partial<S['ServeResponse']> = {}): S['ServeResponse'] {
	return {
		kpi: {
			alerts_fired: null,
			cancel_rate: 0.01,
			facet_sampling_rate: 0.2,
			latency_p50_ms: 8,
			latency_p95_ms: 95,
			notification_failures: null,
			notifications_delivered: null,
			searches: 8642,
			sends: null,
			zero_result_rate: 0.07
		},
		range,
		stale: null,
		top_queries: null,
		top_zero_result_queries: null,
		trend: pts((i) => ({ at: at(i), searches: 700 + i, zero_results: 50 })),
		updated_at: NOW,
		...over
	};
}
