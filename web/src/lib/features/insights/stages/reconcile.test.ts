// The stage pages against the overview (UI review 2026-11-13, 03-q). Which numbers should agree comes
// from docs/metrics.md and was checked by reading the server (crates/api/src/insights.rs and
// insights/stages.rs): discover `harvested` = strip Discover (cohort first harvests, approximate),
// fetch `fetched` = strip Fetch, understand `stored` = strip Understand, index `searchable` = strip
// Index. This test guards the client side of that: each page's KPI is built from the same field the
// strip shows, and both display the same text (the strip's exact count, the page's formatter).
// Serve is not here: the strip is null until the search counters are read (metrics.md).
import { render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';
import { setLocale } from '$lib/paraglide/runtime.js';
import StageStrip from '../overview/StageStrip.svelte';
import { overviewFixture } from '../overview/fixtures';
import { discoverFixture, fetchFixture, indexFixture, understandFixture } from './fixtures';
import { formatMetric, mapDiscover, mapFetch, mapIndex, mapUnderstand, type Metric } from './model';

const o = overviewFixture({
	stages: { discover: 91_234, fetch: 41_077, index: 1_234_567, serve: null, understand: 40_012 }
});
const find = (list: Metric[], key: string) => list.find((x) => x.key === key)!;
const shown = (stage: string) => screen.getByTestId(`insights-stage-${stage}-exact`).textContent;

afterEach(() => setLocale('en', { reload: false }));

describe('stage KPIs show what the overview strip shows', () => {
	it.each(['en', 'zh'] as const)('discover, fetch, understand and index in %s', (locale) => {
		setLocale(locale, { reload: false });
		render(StageStrip, { stages: o.stages, onjump: () => {} });
		const rows: [string, Metric][] = [
			[
				'discover',
				find(
					mapDiscover(
						discoverFixture({ kpi: { ...discoverFixture().kpi, harvested: o.stages.discover } })
					).kpis,
					'harvested'
				)
			],
			[
				'fetch',
				find(
					mapFetch(fetchFixture({ kpi: { ...fetchFixture().kpi, fetched: o.stages.fetch! } })).kpis,
					'fetched'
				)
			],
			[
				'understand',
				find(
					mapUnderstand(
						understandFixture({ kpi: { ...understandFixture().kpi, stored: o.stages.understand } })
					).kpis,
					'stored'
				)
			],
			[
				'index',
				find(
					mapIndex(indexFixture({ kpi: { ...indexFixture().kpi, searchable: o.stages.index } }))
						.kpis,
					'searchable'
				)
			]
		];
		for (const [stage, x] of rows) expect(formatMetric(x, x.value, locale)).toBe(shown(stage));
	});

	it('the Discover figure is approximate and the Index one is a snapshot, as the strip says', () => {
		expect(find(mapDiscover(discoverFixture()).kpis, 'harvested').approx).toBe(true);
		expect(find(mapIndex(indexFixture()).kpis, 'searchable').snapshot).toBe(true);
	});
});
