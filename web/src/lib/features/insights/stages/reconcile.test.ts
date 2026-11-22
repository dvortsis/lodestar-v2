// The stage pages against the overview (UI review 2026-11-13, 03-q). One response per page is built
// from the overview's own strip numbers; the page's KPI must be that same number, through the same
// formatter, and the pairs that differ by time base say so. Pairs, from docs/metrics.md:
//   discover: stage `harvested` = strip Discover (first harvest, ≈)
//   fetch:    `fetched` = strip Fetch (event time)
//   understand: `stored` = strip Understand (event time)
//   index:    `searchable` = strip Index = Searchable KPI (snapshot)
//   serve:    `searches` = strip Serve (event time)
import { describe, expect, it } from 'vitest';
import { formatCount } from '../../../format';
import { overviewFixture } from '../overview/fixtures';
import {
	discoverFixture,
	fetchFixture,
	indexFixture,
	serveFixture,
	understandFixture
} from './fixtures';
import { mapDiscover, mapFetch, mapIndex, mapServe, mapUnderstand, type Metric } from './model';

const o = overviewFixture({
	stages: { discover: 91_234, fetch: 41_077, index: 1_234_567, serve: 5_555, understand: 40_012 }
});
const find = (list: Metric[], key: string) => list.find((x) => x.key === key)!;
const same = (x: Metric, strip: number | null) => {
	expect(x.value).toBe(strip);
	expect(formatCount(x.value)).toBe(formatCount(strip));
};

describe('stage KPIs agree with the overview strip', () => {
	it('discover: harvested = strip Discover, and it is approximate', () => {
		const m = find(
			mapDiscover(
				discoverFixture({ kpi: { ...discoverFixture().kpi, harvested: o.stages.discover } })
			).kpis,
			'harvested'
		);
		same(m, o.stages.discover);
		expect(m.approx).toBe(true);
	});
	it('fetch: fetched = strip Fetch', () => {
		same(
			find(
				mapFetch(fetchFixture({ kpi: { ...fetchFixture().kpi, fetched: o.stages.fetch! } })).kpis,
				'fetched'
			),
			o.stages.fetch
		);
	});
	it('understand: stored = strip Understand', () => {
		same(
			find(
				mapUnderstand(
					understandFixture({ kpi: { ...understandFixture().kpi, stored: o.stages.understand } })
				).kpis,
				'stored'
			),
			o.stages.understand
		);
	});
	it('index: searchable = strip Index = the Searchable KPI, and it is a snapshot', () => {
		const m = find(
			mapIndex(indexFixture({ kpi: { ...indexFixture().kpi, searchable: o.stages.index } })).kpis,
			'searchable'
		);
		same(m, o.stages.index);
		expect(o.stages.index).toBe(o.kpi.searchable);
		expect(m.snapshot).toBe(true);
	});
	it('serve: searches = strip Serve', () => {
		same(
			find(
				mapServe(serveFixture({ kpi: { ...serveFixture().kpi, searches: o.stages.serve! } })).kpis,
				'searches'
			),
			o.stages.serve
		);
	});
});
