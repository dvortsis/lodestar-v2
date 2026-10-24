// Test data typed from the generated schema, shared by the facet rail's tests.
import type { FacetsResponse, TermFacet } from './facets.svelte';

export const DAY = 86_400;
export const NOW = 1_800_000_000;

export const term = (
	values: [string, number, ('selected' | 'excluded')?][],
	over: Partial<TermFacet> = {}
): TermFacet => ({
	approx: false,
	distinct: values.length,
	other: 0,
	values: values.map(([value, count, state]) => ({
		value,
		count,
		selected: state === 'selected',
		excluded: state === 'excluded'
	})),
	...over
});

const GiB = 2 ** 30;

export const response = (over: Partial<FacetsResponse> = {}): FacetsResponse => ({
	backfill_before: NOW - 20 * DAY,
	buckets: { seeders: null, trackers: null },
	canonical: 'lantern',
	facets: {
		type: term([
			['movie', 812],
			['tv', 301],
			['music', 90]
		]),
		res: term([
			['1080p', 640],
			['2160p', 210],
			['720p', 120],
			['480p', 33]
		]),
		ext: term(
			Array.from({ length: 8 }, (_, i): [string, number] => [`e${i}`, 100 - i]),
			{ distinct: 30, other: 220 }
		),
		lang: term([['en', 900]]),
		private: term([
			['yes', 12],
			['no', 1272]
		])
	},
	histograms: {
		size: {
			approx: false,
			missing: 0,
			unit: 'bytes',
			bins: [
				{ lo: 0, hi: GiB, count: 300, backfill: false },
				{ lo: GiB, hi: 4 * GiB, count: 700, backfill: false },
				{ lo: 4 * GiB, hi: 16 * GiB, count: 0, backfill: false },
				{ lo: 16 * GiB, hi: null, count: 284, backfill: false }
			]
		},
		discovered: {
			approx: false,
			missing: 0,
			unit: 'week',
			bins: [
				{ lo: NOW - 28 * DAY, hi: NOW - 21 * DAY, count: 500, backfill: true },
				{ lo: NOW - 21 * DAY, hi: NOW - 14 * DAY, count: 400, backfill: false },
				{ lo: NOW - 14 * DAY, hi: NOW - 7 * DAY, count: 200, backfill: false },
				{ lo: NOW - 7 * DAY, hi: NOW, count: 184, backfill: false }
			]
		}
	},
	sample_rate: null,
	sampled: false,
	took_ms: 3,
	total: 1284,
	total_approx: false,
	unavailable: ['seeders', 'trackers'],
	...over
});
