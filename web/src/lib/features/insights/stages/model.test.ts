import { describe, expect, it } from 'vitest';
import {
	binEdges,
	formatMetric,
	fetchBars,
	discoverBars,
	discoverTables,
	mapDiscover,
	mapFetch,
	mapIndex,
	mapServe,
	mapUnderstand,
	type Metric
} from './model';
import {
	discoverFixture,
	fetchFixture,
	indexFixture,
	serveFixture,
	understandFixture
} from './fixtures';

const find = (list: Metric[], key: string) => list.find((x) => x.key === key)!;

describe('formatMetric', () => {
	const x = (fmt: Metric['fmt'], approx = false): Metric => ({
		key: 'k',
		label: 'k',
		value: 0,
		fmt,
		approx
	});
	it('shows a dash for null, never 0', () => {
		for (const f of ['count', 'percent', 'span', 'latency', 'bytes', 'days'] as const) {
			expect(formatMetric(x(f), null)).toBe('—');
		}
	});
	it('puts ≈ on estimates, and always on latency percentiles', () => {
		expect(formatMetric(x('count', true), 1200)).toMatch(/^≈\u00a0/);
		expect(formatMetric(x('latency'), 1400)).toMatch(/^≈\u00a0/);
		expect(formatMetric(x('count'), 1200)).not.toContain('≈');
	});
	it('formats a ratio as a percent and days as a span', () => {
		expect(formatMetric(x('percent'), 0.25)).toBe('25.0%');
		expect(formatMetric(x('days'), 3)).toBe('3 days');
	});
});

describe('mapFetch', () => {
	const model = mapFetch(fetchFixture());
	it('maps the response fields to KPIs', () => {
		expect(find(model.kpis, 'fetched').value).toBe(4321);
		expect(find(model.kpis, 'lost').value).toBe(777);
		expect(find(model.kpis, 'queue_depth').value).toBe(1234);
	});
	it('keeps a missing series as null (not available), not 0', () => {
		expect(find(model.more, 'utp_share').value).toBeNull();
		expect(find(model.more, 'ports').value).toBeNull();
		expect(find(model.more, 'mse_share').value).toBe(0.25);
	});
	it('keeps a gap in the trend as null', () => {
		expect(model.charts[0]?.series[0]?.values[5]).toBeNull();
	});
	it('carries the previous period only when compare data is sent and aligned', () => {
		expect(model.charts[0]?.series[0]?.previous).toBeUndefined();
		const base = fetchFixture();
		const withPrev = mapFetch({
			...base,
			previous: { kpi: { ...base.kpi, fetched: 99 }, range: base.range, trend: base.trend }
		});
		expect(find(withPrev.kpis, 'fetched').previous).toBe(99);
		expect(withPrev.charts[0]?.series[0]?.previous).toHaveLength(12);
		const short = mapFetch({
			...base,
			previous: { kpi: base.kpi, range: base.range, trend: base.trend.slice(1) }
		});
		expect(short.charts[0]?.series[0]?.previous).toBeUndefined();
	});
});

describe('mapUnderstand', () => {
	const model = mapUnderstand(understandFixture());
	it('has counts only for the blocklist, and n/a for missing histograms', () => {
		expect(find(model.kpis, 'blocked').value).toBe(13);
		expect(model.hists.find((h) => h.id === 'junk')?.bins).toBeNull();
		expect(model.hists.find((h) => h.id === 'confidence')?.bins).toHaveLength(2);
		expect(find(model.more, 'corrections').value).toBeNull();
	});
});

describe('mapIndex', () => {
	const model = mapIndex(indexFixture());
	it('reads sizes as null without a size snapshot', () => {
		expect(find(model.more, 'db').value).toBeNull();
		expect(find(model.kpis, 'projection').value).toBeNull();
		expect(find(model.kpis, 'searchable').value).toBe(1_234_567);
	});
	it('shows the rebuild status as text', () => {
		expect(model.notes[0]?.text).toBe('Building');
	});
	it('draws the total on its own chart (no dual axis)', () => {
		expect(model.charts).toHaveLength(2);
		expect(model.charts[1]?.series).toHaveLength(1);
	});
});

describe('snapshot series', () => {
	it('flags the total on the Index page as a snapshot chart, and the new/removed chart as not', () => {
		const charts = mapIndex(indexFixture()).charts;
		expect(charts.find((c) => c.id === 'index-total')?.snapshot).toBe(true);
		expect(charts.find((c) => c.id === 'index-trend')?.snapshot).toBeUndefined();
	});
	it('flags readings as snapshot metrics and event counts as not', () => {
		const fetch = mapFetch(fetchFixture());
		expect(find(fetch.kpis, 'queue_depth').snapshot).toBe(true);
		expect(find(fetch.kpis, 'fetched').snapshot).toBe(false);
		const index = mapIndex(indexFixture());
		for (const k of ['searchable', 'lag', 'projection']) {
			expect(find(index.kpis, k).snapshot).toBe(true);
		}
		expect(find(index.kpis, 'new').snapshot).toBe(false);
	});
});

describe('mapServe', () => {
	it('formats the query counts in the given locale', () => {
		const t = mapServe(
			serveFixture({ top_queries: [{ count: 1234, query: 'a', zero: 5 }] }),
			'de'
		).tables;
		expect(t[0]?.rows[0]?.count).toBe('1.234');
	});
	it('says query logging is off when the lists are null', () => {
		const t = mapServe(serveFixture()).tables;
		expect(t.every((x) => x.empty && x.rows.length === 0)).toBe(true);
	});
	it('lists queries when logging is on', () => {
		const t = mapServe(
			serveFixture({ top_queries: [{ count: 1234, query: 'ubuntu', zero: 0 }] })
		).tables;
		expect(t[0]?.rows).toEqual([{ query: 'ubuntu', count: '1,234', zero: '0' }]);
		expect(t[0]?.empty).toBeUndefined();
	});
	it('marks facet sampling as an estimate', () => {
		expect(find(mapServe(serveFixture()).more, 'facets').approx).toBe(true);
	});
});

describe('binEdges', () => {
	it('gives edges for contiguous closed bins only', () => {
		expect(
			binEdges([
				{ count: 1, hi: 50, lo: 0 },
				{ count: 1, hi: 100, lo: 50 }
			])
		).toEqual([0, 50, 100]);
		expect(
			binEdges([
				{ count: 1, hi: 10, lo: 0 },
				{ count: 1, hi: 30, lo: 20 }
			])
		).toBeUndefined();
		expect(binEdges([])).toBeUndefined();
	});
});

describe('fetchBars', () => {
	it('marks a null loss list as unavailable, not as no losses', () => {
		const [b] = fetchBars(fetchFixture({ losses: null }), (k) => k);
		expect(b?.unavailable).toBe(true);
		expect(b?.items).toEqual([]);
	});
	it('keeps an empty list as available, and carries each share', () => {
		expect(fetchBars(fetchFixture({ losses: [] }), (k) => k)[0]?.unavailable).toBe(false);
		const first = fetchBars(fetchFixture(), (k) => k)[0]?.items[0];
		expect(first).toMatchObject({ key: 'no_peers', text: '500', sub: '65%' });
	});
});

describe('mapDiscover', () => {
	const model = mapDiscover(discoverFixture());
	it('marks unique harvested as an estimate, and reported as exact', () => {
		expect(find(model.kpis, 'harvested').approx).toBe(true);
		expect(find(model.kpis, 'reported').approx).toBe(false);
		expect(formatMetric(find(model.kpis, 'harvested'), 8765)).toMatch(/^≈\u00a0/);
	});
	it('keeps unmeasured series as null (not available)', () => {
		expect(find(model.more, 'bep51').value).toBeNull();
		expect(find(model.more, 'keyspace').value).toBeNull();
		expect(find(model.more, 'seen_filter').value).toBeNull();
		expect(find(model.kpis, 'reply_ratio').value).toBe(0.75);
	});
	it('draws harvested and reported on separate charts (no dual axis), gaps as null', () => {
		expect(model.charts.map((c) => c.id)).toEqual(['discover-harvested', 'discover-reported']);
		expect(model.charts[0]?.series[0]?.values[5]).toBeNull();
	});
	it('labels known strategies and keeps unknown (plugin) keys as they are', () => {
		const bars = discoverBars(
			discoverFixture({
				by_strategy: [
					{ key: 'sample', reported: 1 },
					{ key: 'x:1', reported: 2 }
				]
			})
		)[0];
		expect(bars?.items.map((i) => i.label)).toEqual(['BEP 51 sample', 'x:1']);
	});
	it('says the IP-family split is not available when null', () => {
		const t = discoverTables(discoverFixture()).find((x) => x.id === 'by-ip-family');
		expect(t?.empty).toBeTruthy();
		expect(t?.rows).toEqual([]);
	});
});
