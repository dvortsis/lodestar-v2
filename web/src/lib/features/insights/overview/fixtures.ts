// Hand-built responses for the unit and e2e tests. The values are distinctive on purpose, so a
// test can tell a field from a number the client made up. Relative imports only: Playwright
// loads this file without the `$lib` alias. Never imported by the app.
import type { components } from '../../../api/schema';

type Overview = components['schemas']['OverviewResponse'];
type Crawler = components['schemas']['CrawlerStatus'];
type Trend = components['schemas']['TrendPoint'];

/** 2026-10-23 12:00 UTC, the fixture's "now". */
export const NOW = 1_792_756_800;
const HOUR = 3600;

function trend(points: number, offset = 0): Trend[] {
	return Array.from({ length: points }, (_, i) => ({
		at: NOW - (points - i) * HOUR - offset,
		searchable: i === 7 || i === 8 ? null : 100 + i * 7,
		harvested: i === 7 || i === 8 ? null : 900 + i * 13,
		searchable_imported: i === 7 || i === 8 ? null : 0
	}));
}

export function overviewFixture(over: Partial<Overview> = {}): Overview {
	return {
		annotations: [
			{ at: NOW - 30 * HOUR, kind: 'restart', text: '2.4.1', import: null },
			{ at: NOW - 20 * HOUR, kind: 'paused', text: '', import: null },
			{ at: NOW - 18 * HOUR, kind: 'resumed', text: '', import: null },
			{ at: NOW - 10 * HOUR, kind: 'intensity', text: 'aggressive', import: null }
		],
		exceptions: [
			{
				fix: { kind: 'settings', target: 'crawler' },
				kind: 'cohort_stalled',
				severity: 'warning',
				since: NOW - 4 * HOUR,
				unit: 'torrents',
				value: 4242
			},
			{
				fix: { kind: 'stage', target: 'index' },
				kind: 'index_lag',
				severity: 'error',
				since: NOW - 2 * HOUR,
				unit: 's',
				value: 95
			}
		],
		funnel: [
			{
				cohort: NOW - 3 * HOUR,
				conversion: { fetched: 0.5, peer_found: 0.6, searchable: 0.9, stored: 0.95 },
				final: false,
				in_progress: 120,
				losses: { no_peers: 10 },
				stages: { fetched: 500, harvested: 1000, peer_found: 600, searchable: 405, stored: 450 }
			},
			{
				cohort: NOW - 4 * HOUR,
				conversion: { fetched: 0.55, peer_found: 0.65, searchable: 0.97, stored: 0.98 },
				final: true,
				in_progress: 0,
				losses: { no_peers: 300, timed_out: 100 },
				stages: { fetched: 550, harvested: 1100, peer_found: 715, searchable: 520, stored: 540 }
			}
		],
		funnel_total: {
			cohorts: 61,
			conversion: { fetched: 0.6821, peer_found: 0.6612, searchable: 0.9965, stored: 0.974 },
			final_cohorts: 59,
			in_progress: 1500,
			loss_share: { gave_up: 0.1, no_peers: 0.6, timed_out: 0.3, refused: 0 },
			losses: { gave_up: 1000, no_peers: 6000, timed_out: 3000, refused: 0 },
			stages: {
				fetched: 41077,
				harvested: 91234,
				peer_found: 60321,
				searchable: 39876,
				stored: 40012
			}
		},
		ingest_per_hour: 321,
		kpi: {
			health: { reason: null, state: 'running' },
			imported: 4321,
			new_searchable: 7891,
			new_searchable_delta: null,
			new_searchable_imported: 0,
			searchable: 1_234_567,
			searchable_crawled: 1_230_246,
			searchable_delta: null,
			time_to_searchable_p50_delta_s: null,
			time_to_searchable_p50_s: 5400,
			time_to_searchable_p95_delta_s: null,
			time_to_searchable_p95_s: 90_000,
			yield: 0.4371,
			yield_delta: null
		},
		range: { bucket: 'hour', from: NOW - 48 * HOUR, to: NOW, tz: 'UTC' },
		segments: {
			by_content_type: [
				{ key: 'movie', searchable: 3000 },
				{ key: 'tv', searchable: 2500 },
				{ key: 'music', searchable: 1200 },
				{ key: 'unclassified', searchable: 77 }
			],
			by_group: [
				{ key: 'video', searchable: 5500 },
				{ key: 'audio', searchable: 1200 },
				{ key: 'unclassified', searchable: 77 }
			],
			by_source: [
				{ harvested: 70000, key: 'dht', searchable: 31000, yield: 0.4429 },
				{ harvested: 21234, key: 'tracker', searchable: 8876, yield: 0.418 }
			]
		},
		serve: {
			alerts_fired: null,
			latency_p95_ms: null,
			searches: null,
			sends: null,
			zero_result_rate: null
		},
		stages: { discover: 91_234, fetch: 41_077, index: 1_234_567, serve: null, understand: 40_012 },
		stale: null,
		trend: trend(48),
		updated_at: NOW - 40,
		...over
	};
}

export function crawlerFixture(over: Partial<Crawler> = {}): Crawler {
	return {
		dht: null,
		fetch: { in_flight: 0, lookups: 0, queued: 0 },
		intensity: 'balanced',
		intensity_env: null,
		paused: false,
		since: NOW - 86_400,
		state: 'running',
		...over
	};
}

/** The deltas the server adds with `compare=1`. */
export const COMPARE_KPI = {
	new_searchable_delta: 1234,
	searchable_delta: 56_789,
	yield_delta: -0.032
} as const;
