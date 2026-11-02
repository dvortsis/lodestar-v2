import { render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

afterEach(() => vi.unstubAllGlobals());
import {
	formatCompact,
	formatCount,
	formatDelta,
	formatLatency,
	formatPercent,
	formatSpan
} from '$lib/format';
import Exceptions from './Exceptions.svelte';
import Funnel from './Funnel.svelte';
import { COMPARE_KPI, IMPORT_ANNOTATIONS, overviewFixture } from './fixtures';
import ImportMarkers from './ImportMarkers.svelte';
import { importText } from './imports';
import Movement from './Movement.svelte';
import KpiRow from './KpiRow.svelte';
import Segments from './Segments.svelte';
import ServePanel from './ServePanel.svelte';
import StageStrip from './StageStrip.svelte';
import type { InsightsQuery } from './query';

const q: InsightsQuery = { range: '7d', compare: false };
const o = overviewFixture();
const withDeltas = { ...o.kpi, ...COMPARE_KPI };
const text = (id: string) => screen.getByTestId(id).textContent?.trim();
const approx = (s: string) => `≈\u00a0${s}`;

describe('every number is a response field through a formatter', () => {
	it('stage strip', () => {
		render(StageStrip, { stages: o.stages, onjump: () => {} });
		expect(text('insights-stage-discover-value')).toBe(approx(formatCompact(o.stages.discover)));
		expect(text('insights-stage-fetch-value')).toBe(formatCompact(o.stages.fetch));
		expect(text('insights-stage-index-exact')).toBe(formatCount(o.stages.index));
		expect(text('insights-stage-discover-exact')).toBe(approx(formatCount(o.stages.discover)));
		// A null is a dash with an accessible name, never 0.
		expect(text('insights-stage-serve-value')).toBe('—');
		expect(screen.getByTestId('insights-stage-serve').textContent).toContain('Not measured yet');
	});

	it('names why search is off', () => {
		render(StageStrip, { stages: { ...o.stages, index: null }, onjump: () => {} });
		expect(screen.getByTestId('insights-stage-index').textContent).toContain('Search is off');
	});

	it('KPI row', () => {
		render(KpiRow, { kpi: withDeltas });
		const k = withDeltas;
		expect(text('insights-kpi-searchable-value')).toBe(formatCompact(k.searchable));
		expect(text('insights-kpi-searchable-delta')).toContain(
			formatDelta(k.searchable_delta, 'en', 'compact')
		);
		expect(text('insights-kpi-imported')).toBe(formatCount(k.imported));
		expect(text('insights-kpi-new-value')).toBe(formatCompact(k.new_searchable));
		expect(text('insights-kpi-yield-value')).toBe(formatPercent(k.yield, 'en', 1));
		expect(text('insights-kpi-yield-delta')).toContain(formatDelta(k.yield_delta, 'en', 'percent'));
		expect(text('insights-kpi-time-value')).toBe(
			formatSpan({ value: k.time_to_searchable_p50_s, approx: true })
		);
		expect(text('insights-kpi-time-p95')).toBe(
			formatSpan({ value: k.time_to_searchable_p95_s, approx: true })
		);
	});

	it('shows yield and time as a dash with a reason when no cohort is final, and no deltas without compare', () => {
		const kpi = {
			...o.kpi,
			yield: null,
			yield_delta: null,
			time_to_searchable_p50_s: null,
			time_to_searchable_p95_s: null,
			searchable_delta: null,
			new_searchable_delta: null
		};
		render(KpiRow, { kpi });
		expect(text('insights-kpi-yield-value')).toBe('—');
		expect(text('insights-kpi-yield-none')).toBe('No completed cohorts yet');
		expect(text('insights-kpi-time-value')).toBe('—');
		expect(text('insights-kpi-time-none')).toBe('No completed cohorts yet');
		expect(screen.queryByTestId('insights-kpi-searchable-delta')).toBeNull();
		expect(screen.queryByTestId('insights-kpi-new-delta')).toBeNull();
	});

	it('keeps a real zero and shows search-off as a dash', () => {
		render(KpiRow, { kpi: { ...o.kpi, new_searchable: 0, searchable: null, imported: null } });
		expect(text('insights-kpi-new-value')).toBe('0');
		expect(text('insights-kpi-searchable-value')).toBe('—');
		expect(text('insights-kpi-imported')).toBe('—');
	});

	it('shows health as icon and text', () => {
		render(KpiRow, { kpi: { ...o.kpi, health: { state: 'degraded', reason: 'index_lag' } } });
		expect(text('insights-kpi-health-state')).toBe('Degraded');
		expect(text('insights-kpi-health-reason')).toBe('The search index is behind.');
		expect(screen.getByTestId('insights-kpi-health-state').querySelector('svg')).not.toBeNull();
	});

	it('funnel: counts, the server conversion, cohort and loss figures', () => {
		render(Funnel, { overview: o, query: q, rangeText: 'the last 7 days' });
		const t = o.funnel_total;
		expect(text('insights-funnel-stored-value')).toBe(formatCount(t.stages.stored));
		expect(text('insights-funnel-stored-sub')).toContain(
			formatPercent(t.conversion.stored, 'en', 1)
		);
		expect(text('insights-funnel-harvested-value')).toBe(formatCount(t.stages.harvested));
		expect(screen.queryByTestId('insights-funnel-harvested-sub')).toBeNull();
		expect(text('insights-funnel-complete')).toContain(
			`${formatCount(t.final_cohorts)} of ${formatCount(t.cohorts)} cohorts complete`
		);
		expect(text('insights-loss-no_peers-count')).toBe(formatCount(t.losses.no_peers));
		expect(text('insights-loss-no_peers-share')).toBe(
			formatPercent(t.loss_share.no_peers, 'en', 0)
		);
		// A reason that lost nothing is not listed.
		expect(screen.queryByTestId('insights-loss-refused')).toBeNull();
	});

	it('funnel: an incomplete cohort is hatched and labelled', () => {
		render(Funnel, { overview: o, query: q, rangeText: 'x' });
		const open = o.funnel.find((c) => !c.final)!;
		const row = screen.getByTestId(`insights-cohort-${open.cohort}`);
		expect(row.textContent).toContain('Incomplete');
		expect(row.querySelector('rect[fill="url(#ins-hatch)"]')).not.toBeNull();
		const done = o.funnel.find((c) => c.final)!;
		const solid = screen.getByTestId(`insights-cohort-${done.cohort}`);
		expect(solid.textContent).not.toContain('Incomplete');
		expect(solid.querySelector('rect')).toBeNull();
	});

	it('funnel: drill-throughs carry the range and the reason', () => {
		render(Funnel, { overview: o, query: { range: '30d', compare: false }, rangeText: 'x' });
		const hrefs = (id: string) => screen.getByTestId(id).querySelector('a')!.getAttribute('href');
		expect(hrefs('insights-funnel-fetched')).toBe('/insights/fetch?range=30d');
		expect(hrefs('insights-funnel-searchable')).toBe('/insights/index?range=30d');
		expect(hrefs('insights-loss-timed_out')).toBe('/insights/fetch?range=30d&reason=timed_out');
	});

	it('segments: counts and yields, links, and no link for unclassified', () => {
		render(Segments, { segments: o.segments, query: q, rangeText: 'x' });
		expect(text('insights-group-video-value')).toBe(formatCount(5500));
		expect(text('insights-source-dht-value')).toBe(formatPercent(0.4429, 'en', 1));
		const link = (id: string) => screen.getByTestId(id).querySelector('a');
		expect(decodeURIComponent(link('insights-group-video')!.getAttribute('href')!)).toBe(
			'/search?q=type:movie,tv,video discovered:<7d'
		);
		expect(link('insights-group-unclassified')).toBeNull();
		expect(link('insights-type-unclassified')).toBeNull();
		expect(link('insights-source-dht')!.getAttribute('href')).toBe('/insights/discover?range=7d');
		expect(text('insights-segments-note')).toContain('Search total can differ');
	});

	it('segments: no adult row unless the server sent one', () => {
		render(Segments, { segments: o.segments, query: q, rangeText: 'x' });
		expect(screen.queryByTestId('insights-group-adult')).toBeNull();
	});

	it('serve: every null is a dash with "Not measured yet"', () => {
		render(ServePanel, { serve: o.serve, query: q });
		for (const id of ['searches', 'zero', 'latency', 'alerts', 'sends']) {
			expect(text(`insights-serve-${id}-value`)).toBe('—');
		}
		expect(screen.getByTestId('insights-serve').textContent).toContain('Not measured yet');
		expect(screen.getByTestId('insights-serve-zero-link').getAttribute('href')).toBe(
			'/insights/serve?range=7d'
		);
	});

	it('serve: formats real values', () => {
		const serve = {
			searches: 1234,
			zero_result_rate: 0.123,
			latency_p95_ms: 45,
			alerts_fired: 0,
			sends: 7
		};
		render(ServePanel, { serve, query: q });
		expect(text('insights-serve-searches-value')).toBe(formatCount(1234));
		expect(text('insights-serve-zero-value')).toBe(formatPercent(0.123, 'en', 1));
		expect(text('insights-serve-latency-value')).toBe(formatLatency({ value: 45, approx: true }));
		expect(text('insights-serve-alerts-value')).toBe('0');
	});
});

describe('Exceptions', () => {
	it('lists errors first with a message per kind, formatted values and fix links', () => {
		render(Exceptions, { exceptions: o.exceptions });
		const rows = screen.getByTestId('insights-exceptions').querySelectorAll('li');
		expect(rows[0]!.getAttribute('data-severity')).toBe('error');
		expect(rows[0]!.textContent).toContain(formatSpan(95));
		expect(rows[1]!.textContent).toContain(formatCount(4242));
		expect(screen.getByTestId('insights-exception-index_lag-fix').getAttribute('href')).toBe(
			'/insights/index'
		);
		expect(screen.getByTestId('insights-exception-cohort_stalled-fix').getAttribute('href')).toBe(
			'/settings/crawler'
		);
	});

	it('names unreadable filter rows and links to Filters & Safety', () => {
		render(Exceptions, {
			exceptions: [
				{
					kind: 'filter_rows_unreadable',
					severity: 'error',
					since: 1,
					unit: 'rows',
					value: 2,
					rows: ['C-2', 'U-3'],
					fix: { kind: 'settings', target: 'filters' }
				}
			]
		});
		const row = screen.getByTestId('insights-exception-filter_rows_unreadable');
		expect(row.textContent).toContain("Saved filter rows that can't be read: 2 (C-2, U-3)");
		expect(
			screen.getByTestId('insights-exception-filter_rows_unreadable-fix').getAttribute('href')
		).toBe('/settings/filters');
	});

	it('counts what was judged again once filter rows read again', () => {
		render(Exceptions, {
			exceptions: [
				{
					kind: 'filter_stand_ins_rejudged',
					severity: 'warning',
					since: 1,
					unit: 'torrents',
					value: 12,
					rows: ['3', '4'],
					fix: { kind: 'settings', target: 'filters' }
				}
			]
		});
		const row = screen.getByTestId('insights-exception-filter_stand_ins_rejudged');
		expect(row.textContent).toContain('12 released, 3 still held');
		expect(row.textContent).toContain('Filters & Safety: 4');
	});

	it('says so when nothing needs attention', () => {
		render(Exceptions, { exceptions: [] });
		expect(text('insights-exceptions-empty')).toBe('Nothing needs attention');
	});

	it('shows an unknown kind generically, and links nothing for a bad target', () => {
		render(Exceptions, {
			exceptions: [
				{
					kind: 'new_thing',
					severity: 'warning',
					since: 1,
					unit: null,
					value: null,
					fix: { kind: 'settings', target: '../etc' }
				}
			]
		});
		expect(text('insights-exception-new_thing')).toContain('Needs attention: new_thing');
		expect(screen.queryByTestId('insights-exception-new_thing-fix')).toBeNull();
	});
});

describe('imports (IMP-5)', () => {
	it('splits the total into crawled and imported', () => {
		render(KpiRow, { kpi: o.kpi });
		expect(text('insights-kpi-crawled')).toBe(formatCount(o.kpi.searchable_crawled));
		expect(text('insights-kpi-imported')).toBe(formatCount(o.kpi.imported));
	});
	it('shows "+ N imported" under New searchable only above zero, apart from the headline', () => {
		render(KpiRow, { kpi: { ...o.kpi, new_searchable_imported: 250_000 } });
		expect(text('insights-kpi-new-imported')).toBe('+ 250,000 imported');
		expect(text('insights-kpi-new-value')).toBe(formatCompact(o.kpi.new_searchable));
	});
	it('shows no imported line for zero', () => {
		render(KpiRow, { kpi: o.kpi });
		expect(screen.queryByTestId('insights-kpi-new-imported')).toBeNull();
	});
	it("notes an import in the total's change only with a finished marker and a delta", () => {
		const kpi = { ...o.kpi, searchable_delta: 5 };
		const { unmount } = render(KpiRow, { kpi, annotations: IMPORT_ANNOTATIONS });
		expect(text('insights-kpi-delta-import')).toBe('Includes an import');
		unmount();
		render(KpiRow, { kpi, annotations: [IMPORT_ANNOTATIONS[0]!] });
		expect(screen.queryByTestId('insights-kpi-delta-import')).toBeNull();
	});
	it('words each marker with its counts', () => {
		expect(importText(IMPORT_ANNOTATIONS[0]!, 'en')).toBe('Import started');
		expect(importText(IMPORT_ANNOTATIONS[1]!, 'en')).toBe(
			'Import finished: 250,000 new, 1,234 already known'
		);
		expect(importText(IMPORT_ANNOTATIONS[2]!, 'en')).toBe('Import removed: 4,321 deleted');
	});
	it('makes restart and pause notes keyboard-reachable too', () => {
		vi.stubGlobal(
			'ResizeObserver',
			class {
				observe() {}
				disconnect() {}
				unobserve() {}
			}
		);
		render(Movement, { overview: o, query: q, rangeText: 'the last 7 days' });
		for (const kind of ['restart', 'paused']) {
			const li = screen.getByTestId(`insights-annotation-${kind}`);
			expect(li.getAttribute('tabindex')).toBe('0');
			expect(li.getAttribute('aria-label')).toContain(li.querySelector('span')!.textContent!);
		}
	});
	it('lists keyboard-reachable markers, and nothing without an import', () => {
		const { unmount } = render(ImportMarkers, { annotations: IMPORT_ANNOTATIONS, tz: 'UTC' });
		const li = screen.getByTestId('insights-import-marker-import_removed');
		expect(li.getAttribute('tabindex')).toBe('0');
		expect(li.getAttribute('aria-label')).toContain('Import removed: 4,321 deleted');
		unmount();
		render(ImportMarkers, { annotations: o.annotations, tz: 'UTC' });
		expect(screen.queryByTestId('insights-import-markers')).toBeNull();
	});
});
