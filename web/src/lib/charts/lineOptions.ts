// Pure options for the uPlot wrapper, so they can be tested without a canvas. No uPlot import
// here: this file is part of the eager chunk and uPlot itself stays lazy.
import type uPlot from 'uplot';
import { DASH } from '$lib/format';

export interface LineSeries {
	label: string;
	/** One sample per timestamp; `null` is a gap in the line, never zero. */
	values: (number | null)[];
	/** CSS custom property holding the colour, e.g. `--data-1`. Defaults by position. */
	token?: string;
}

export interface LinePalette {
	series: string[];
	text: string;
	grid: string;
}

export interface LineFormat {
	/** Value-axis tick: compact (`1.2K`). */
	axis: (v: number) => string;
	/** Legend and tooltip value: the exact metric formatter. */
	y: (v: number) => string;
	/** Time-axis tick for a time (Unix seconds). */
	tick: (seconds: number) => string;
	/** Legend and tooltip heading: the full absolute date (`formatDateFull`). */
	full: (seconds: number) => string;
}

/** 1, 2, 5, 10, 20, 50, … so a count axis never repeats a label ("0, 1, 1, 2"). */
const INTEGER_STEPS = Array.from({ length: 13 }, (_, e) =>
	[1, 2, 5].map((m) => m * 10 ** e)
).flat();

/** The tokens a chart reads (design-tokens.md §3): primary series `--data-1`, comparison muted. */
export const DEFAULT_TOKENS = [
	'--data-1',
	'--color-muted-text',
	'--data-2',
	'--data-3',
	'--data-4'
];

export function readPalette(el: Element, series: readonly LineSeries[]): LinePalette {
	const css = getComputedStyle(el);
	const get = (name: string) => css.getPropertyValue(name).trim();
	return {
		series: series.map((s, i) => get(s.token ?? DEFAULT_TOKENS[i % DEFAULT_TOKENS.length]!)),
		text: get('--color-muted-text'),
		grid: get('--color-base-300')
	};
}

export function buildData(
	times: readonly number[],
	series: readonly LineSeries[]
): uPlot.AlignedData {
	return [[...times], ...series.map((s) => s.values.map((v) => (v === undefined ? null : v)))];
}

export function buildOptions(args: {
	width: number;
	height: number;
	series: readonly LineSeries[];
	palette: LinePalette;
	format: LineFormat;
	/** Axis unit label ("seeders", "GiB"), shown beside the value axis. */
	unit: string;
	/** CSS font shorthand for the axes; the tokens' body font. */
	font: string;
	/** Counts: the value axis steps by whole numbers only. Default true. */
	integers?: boolean;
}): uPlot.Options {
	const { palette, format } = args;
	const axis = (extra: uPlot.Axis): uPlot.Axis => ({
		stroke: palette.text,
		font: args.font,
		grid: { stroke: palette.grid, width: 1 },
		ticks: { stroke: palette.grid, width: 1 },
		...extra
	});
	return {
		width: args.width,
		height: args.height,
		legend: { live: true },
		cursor: { drag: { x: false, y: false } },
		scales: { x: { time: true }, y: { range: (_u, _min, max) => [0, max > 0 ? max * 1.1 : 1] } },
		axes: [
			axis({ values: (_u, splits) => splits.map((s) => format.tick(s)) }),
			axis({
				label: args.unit,
				labelFont: args.font,
				...(args.integers === false ? {} : { incrs: INTEGER_STEPS }),
				values: (_u, splits) => splits.map((s) => format.axis(s))
			})
		],
		series: [
			{ label: '', value: (_u, v) => (v === null ? DASH : format.full(v)) },
			...args.series.map((s, i) => ({
				label: s.label,
				stroke: palette.series[i],
				width: 2,
				spanGaps: false,
				points: { show: false },
				value: (_u: uPlot, v: number | null) => (v === null ? DASH : format.y(v))
			}))
		]
	};
}
