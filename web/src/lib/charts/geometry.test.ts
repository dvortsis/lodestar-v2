import { describe, expect, it } from 'vitest';
import { clampRange, dragRange, moveEdge, nearestEdge } from './brush';
import { buildData, buildOptions, readPalette, TIME_STEPS } from './lineOptions';
import { lengthPct, numeric } from './shared';
import { sparkPath } from './sparkline';

describe('sparkPath', () => {
	it('breaks the line at nulls instead of zero-filling', () => {
		const g = sparkPath([1, 2, null, 3, 4], 64, 24);
		expect(g.d.match(/M/g)).toHaveLength(2);
		expect(g.points).toBe(4);
	});
	it('draws an isolated sample as a dot', () => {
		const g = sparkPath([null, 5, null], 64, 24);
		expect(g.d).toBe('');
		expect(g.dots).toHaveLength(1);
	});
	it('is empty with no known samples and mid-height when flat', () => {
		expect(sparkPath([null, null], 64, 24)).toEqual({ d: '', dots: [], points: 0 });
		expect(sparkPath([7, 7], 64, 24).d).toContain(' 12');
	});
});

describe('brush', () => {
	const r = { from: 2, to: 5 };
	it('moves edges by one and keeps one bin', () => {
		expect(moveEdge(r, 'from', 'ArrowLeft', 8)).toEqual({ from: 1, to: 5 });
		expect(moveEdge(r, 'to', 'ArrowRight', 8)).toEqual({ from: 2, to: 6 });
		expect(moveEdge({ from: 4, to: 5 }, 'from', 'ArrowRight', 8)).toEqual({ from: 4, to: 5 });
		expect(moveEdge({ from: 4, to: 5 }, 'to', 'ArrowLeft', 8)).toEqual({ from: 4, to: 5 });
	});
	it('clamps to the plot and supports Home/End', () => {
		expect(moveEdge({ from: 0, to: 3 }, 'from', 'ArrowLeft', 8).from).toBe(0);
		expect(moveEdge({ from: 0, to: 8 }, 'to', 'ArrowRight', 8).to).toBe(8);
		expect(moveEdge(r, 'from', 'Home', 8).from).toBe(0);
		expect(moveEdge(r, 'to', 'End', 8).to).toBe(8);
	});
	it('maps pointer positions to the nearest edge and drags either way', () => {
		expect(nearestEdge(0.5, 8)).toBe(4);
		expect(nearestEdge(-1, 8)).toBe(0);
		expect(nearestEdge(2, 8)).toBe(8);
		expect(dragRange(5, 2, 8)).toEqual({ from: 2, to: 5 });
		expect(dragRange(3, 3, 8)).toEqual({ from: 3, to: 4 });
		expect(dragRange(8, 8, 8)).toEqual({ from: 7, to: 8 });
	});
});

describe('clampRange', () => {
	it('fits a stored selection to a smaller bin count and drops it with no bins', () => {
		expect(clampRange({ from: 2, to: 9 }, 5)).toEqual({ from: 2, to: 5 });
		expect(clampRange({ from: 7, to: 9 }, 5)).toEqual({ from: 4, to: 5 });
		expect(clampRange({ from: 0, to: 3 }, 0)).toBeNull();
	});
});

describe('shared', () => {
	it('keeps bar lengths finite and bounded', () => {
		expect(lengthPct(5, 10)).toBe('50%');
		expect(lengthPct(null, 10)).toBe('0%');
		expect(lengthPct(5, 0)).toBe('0%');
		expect(lengthPct(50, 10)).toBe('100%');
	});
	it('reads plain numbers out of metric values', () => {
		expect(numeric({ value: 3, approx: true })).toBe(3);
		expect(numeric(null)).toBeNull();
		expect(numeric(Number.NaN)).toBeNull();
	});
});

describe('line options', () => {
	const series = [{ label: 'Seeders', values: [1, null, 3] }];
	const format = {
		axis: (v: number) => `${v}k`,
		y: (v: number) => `${v}!`,
		tick: (s: number) => `t${s}`,
		full: (s: number) => `T${s}`
	};
	const opts = buildOptions({
		width: 300,
		height: 200,
		series,
		palette: { series: ['red'], text: 'gray', grid: 'silver' },
		format,
		unit: 'seeders',
		font: '12px sans-serif'
	});
	it('keeps nulls as gaps and does not span them', () => {
		expect(buildData([1, 2, 3], series)[1]).toEqual([1, null, 3]);
		expect(opts.series?.[1]?.spanGaps).toBe(false);
	});
	it('shows the unit on the value axis and formats ticks and legend values', () => {
		expect(opts.axes?.[1]?.label).toBe('seeders');
		const values = opts.axes?.[1]?.values as (u: unknown, s: number[]) => string[];
		expect(values(null, [0, 5])).toEqual(['0k', '5k']);
		const full = opts.series?.[0]?.value as (u: unknown, v: number | null) => string;
		expect(full(null, 9)).toBe('T9');
		expect(full(null, null)).toBe('—');
	});
	it('steps counts by whole numbers only, unless told otherwise', () => {
		const incrs = opts.axes?.[1]?.incrs as number[];
		expect(incrs.every((n) => Number.isInteger(n))).toBe(true);
		expect(incrs[0]).toBe(1);
		const free = buildOptions({
			width: 1,
			height: 1,
			series,
			palette: { series: ['red'], text: 'gray', grid: 'silver' },
			format,
			unit: 'ratio',
			font: '12px sans-serif',
			integers: false
		});
		expect(free.axes?.[1]?.incrs).toBeUndefined();
	});
	it('keeps time ticks to whole buckets and spaces them by label width', () => {
		const day = opts.axes?.[0];
		expect(Math.min(...(day?.incrs as number[]))).toBe(86400);
		const week = buildOptions({
			width: 1,
			height: 1,
			series,
			palette: { series: ['red'], text: 'gray', grid: 'silver' },
			format,
			unit: 'u',
			font: '12px sans-serif',
			bucket: 'week'
		});
		expect(Math.min(...(week.axes?.[0]?.incrs as number[]))).toBe(7 * 86400);
		const sampled = buildOptions({
			width: 1,
			height: 1,
			series,
			palette: { series: ['red'], text: 'gray', grid: 'silver' },
			format,
			unit: 'u',
			font: '12px sans-serif',
			sample: 1761000000
		});
		expect(Math.min(...TIME_STEPS.hour)).toBe(3600);
		// "t1761000000" is 11 characters: the minimum gap grows with the label.
		expect(sampled.axes?.[0]?.space).toBeGreaterThan(11 * 6);
		expect(sampled.axes?.[0]?.space).toBeGreaterThan(Number(day?.space));
	});
	it('replaces the English "Time" legend heading with the given text', () => {
		const label = opts.series?.[0]?.label as unknown as HTMLElement;
		expect(label).toBeInstanceOf(HTMLElement);
		expect(label.textContent).toBe('');
	});
	it('starts the value scale at zero', () => {
		const range = opts.scales?.y?.range as unknown as (
			u: unknown,
			min: number,
			max: number
		) => number[];
		expect(range(null, 3, 10)[0]).toBe(0);
		expect(range(null, 0, 0)).toEqual([0, 1]);
	});
	it('reads colours from the element, by token', () => {
		const el = document.createElement('div');
		el.style.setProperty('--data-1', 'rgb(1, 2, 3)');
		el.style.setProperty('--color-muted-text', 'rgb(4, 5, 6)');
		document.body.append(el);
		const p = readPalette(el, [
			{ label: 'a', values: [] },
			{ label: 'b', values: [] }
		]);
		expect(p.series).toEqual(['rgb(1, 2, 3)', 'rgb(4, 5, 6)']);
		el.remove();
	});
});
