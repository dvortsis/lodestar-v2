// Insights in the chosen time zone (X-labels, U28; docs/metrics.md §1–2): instants and hour buckets
// in the zone Language & region chose (the browser's for Automatic), day buckets as the UTC days
// the server counts, named "UTC". And sources and types by name.
import { render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { bucketTimeZone, formatDateFull } from '$lib/format';
import { displayTimeZone, setZone } from '$lib/i18n/locale.svelte';
import Funnel from './Funnel.svelte';
import { overviewFixture } from './fixtures';
import Movement from './Movement.svelte';
import Segments from './Segments.svelte';
import type { InsightsQuery } from './query';

const q: InsightsQuery = { range: '7d', compare: false };
const HONOLULU = 'Pacific/Honolulu'; // UTC−10, no daylight saving

afterEach(() => {
	setZone('');
	vi.unstubAllGlobals();
});

describe('bucketTimeZone', () => {
	it('shows hours and instants in the display zone, days and weeks in UTC', () => {
		expect(bucketTimeZone('hour', HONOLULU)).toBe(HONOLULU);
		expect(bucketTimeZone(undefined, HONOLULU)).toBe(HONOLULU);
		expect(bucketTimeZone('day', HONOLULU)).toBe('UTC');
		expect(bucketTimeZone('week', HONOLULU)).toBe('UTC');
		expect(bucketTimeZone('hour', 'UTC')).toBe('UTC');
	});
	it('Automatic is the browser’s zone; a chosen zone wins', () => {
		setZone('');
		expect(displayTimeZone()).toBe(Intl.DateTimeFormat().resolvedOptions().timeZone);
		setZone(HONOLULU);
		expect(displayTimeZone()).toBe(HONOLULU);
	});
});

describe('the funnel’s cohorts', () => {
	it('labels hourly cohorts in the chosen zone, across a date boundary', () => {
		setZone(HONOLULU);
		const o = overviewFixture();
		expect(o.range.bucket).toBe('hour');
		render(Funnel, { overview: o, query: q, rangeText: 'x' });
		// A cohort late in the UTC day falls on the previous date in Honolulu.
		const c = o.funnel[0]!;
		const row = screen.getByTestId(`insights-cohort-${c.cohort}`);
		const there = formatDateFull(c.cohort, 'en', { timeZone: HONOLULU, bucket: 'hour' });
		expect(row.textContent).toContain(there);
		expect(there).not.toBe(formatDateFull(c.cohort, 'en', { timeZone: 'UTC', bucket: 'hour' }));
	});
	it('labels daily cohorts as UTC days, whatever the chosen zone', () => {
		setZone(HONOLULU);
		const o = overviewFixture();
		const daily = { ...o, range: { ...o.range, bucket: 'day' as const } };
		render(Funnel, { overview: daily, query: q, rangeText: 'x' });
		const c = daily.funnel[0]!;
		const row = screen.getByTestId(`insights-cohort-${c.cohort}`);
		expect(row.textContent).toContain(
			formatDateFull(c.cohort, 'en', { timeZone: 'UTC', bucket: 'day' })
		);
	});
});

describe('Movement’s annotations', () => {
	it('shows an annotation’s time in the chosen zone', () => {
		vi.stubGlobal(
			'ResizeObserver',
			class {
				observe() {}
				disconnect() {}
				unobserve() {}
			}
		);
		setZone(HONOLULU);
		const o = overviewFixture();
		render(Movement, { overview: o, query: q, rangeText: 'x' });
		const li = screen.getByTestId('insights-annotation-restart');
		const a = o.annotations.find((x) => x.kind === 'restart')!;
		expect(li.textContent).toContain(formatDateFull(a.at, 'en', { timeZone: HONOLULU }));
		expect(li.textContent).toContain('HST');
	});
});

describe('segments by name', () => {
	it('names a source by its plugin name and types by the glossary', () => {
		render(Segments, { segments: overviewFixture().segments, query: q, rangeText: 'x' });
		expect(screen.getByTestId('insights-source-dht').textContent).toContain('DHT');
		expect(screen.getByTestId('insights-source-dht').textContent).not.toMatch(/\bdht\b/);
		// An id with no name shows as sent.
		expect(screen.getByTestId('insights-source-tracker').textContent).toContain('tracker');
	});
});
