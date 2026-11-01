<script lang="ts">
	import { onMount } from 'svelte';
	import type uPlot from 'uplot';
	import {
		formatCompact,
		formatCount,
		formatDate,
		formatDateFull,
		formatDateTime,
		type MetricValue
	} from '$lib/format';
	import { currentLocale, displayTimeZone } from '$lib/i18n/locale.svelte';
	import {
		buildData,
		buildOptions,
		readPalette,
		type LineFormat,
		type LineSeries
	} from './lineOptions';
	import type { ValueFormatter } from './shared';

	interface Props {
		/** Sample times, Unix seconds, ascending. */
		times: number[];
		series: LineSeries[];
		/** Accessible description (a one-sentence summary; pair with a DataTableAlternative). */
		label: string;
		/** Axis unit, shown beside the value axis. */
		unit: string;
		/** Legend and tooltip values: the exact metric formatter. */
		format?: ValueFormatter;
		/** Value-axis ticks; compact by default (`1.2K`). */
		axisFormat?: ValueFormatter;
		/** Series resolution. Day and week ticks show the date; hour ticks include the time. */
		bucket?: 'hour' | 'day' | 'week';
		/** Counts step by whole numbers on the value axis. Turn off for rates and ratios. */
		integers?: boolean;
		/** IANA time zone, the user's setting. */
		timeZone?: string;
		/** Heading of the legend's date row, e.g. "Date". Empty by default. */
		timeLabel?: string;
		/** uPlot failed to load (offline, blocked chunk). The container stays empty. */
		onerror?: (error: unknown) => void;
		height?: number;
		testid?: string;
	}
	let {
		times,
		series,
		label,
		unit,
		format = formatCount,
		axisFormat = formatCompact,
		bucket = 'day',
		integers = true,
		timeZone,
		timeLabel,
		onerror,
		height = 240,
		testid = 'line-chart'
	}: Props = $props();

	let host: HTMLElement;
	let plot: uPlot | undefined;
	let ready = $state(false);
	let failed = $state(false);
	let create: (() => void) | undefined;

	const locale = $derived(currentLocale());
	// The display zone is read here so a change to the setting rebuilds the labels.
	const zone = $derived(timeZone ?? displayTimeZone());
	const fmt = $derived<LineFormat>({
		axis: (v) => axisFormat(v as MetricValue, locale),
		y: (v) => format(v as MetricValue, locale),
		tick: (s) =>
			bucket === 'hour'
				? formatDateTime(s, locale, { timeZone: zone })
				: formatDate(s, locale, { timeZone: zone }),
		full: (s) => formatDateFull(s, locale, { timeZone: zone, bucket })
	});
	// Anything that changes how the chart is built (not just what it plots) forces a rebuild.
	const shape = $derived(
		JSON.stringify([
			series.map((s) => [s.label, s.token]),
			unit,
			height,
			integers,
			bucket,
			timeLabel
		])
	);

	function destroy() {
		plot?.destroy();
		plot = undefined;
	}

	// Rebuild only when the shape, the formatting or the theme changes. New samples go through
	// setData and redraw in place; uPlot has no animation, so nothing re-animates (motion-ui) and
	// reduced motion needs no special case.
	$effect(() => {
		void [shape, fmt];
		create?.();
	});
	$effect(() => {
		const data = buildData(times, series);
		if (plot) plot.setData(data);
	});

	onMount(() => {
		let disposed = false;
		let width = host.clientWidth;

		const mount = (UPlot: typeof uPlot) => {
			destroy();
			if (disposed) return;
			const css = getComputedStyle(host);
			plot = new UPlot(
				buildOptions({
					width: Math.max(width, 1),
					height,
					series,
					palette: readPalette(host, series),
					format: fmt,
					unit,
					integers,
					bucket,
					sample: times.at(-1),
					timeLabel,
					font: `${css.fontSize} ${css.fontFamily}`
				}),
				buildData(times, series),
				host
			);
			ready = true;
		};

		// uPlot and its stylesheet load on first mount only: they stay out of the first-load chunk.
		const loaded = Promise.all([import('uplot'), import('uplot/dist/uPlot.min.css')]).then(
			([m]) => m.default
		);
		loaded.then(
			(UPlot) => {
				create = () => mount(UPlot);
				create();
			},
			(error) => {
				failed = true;
				onerror?.(error);
			}
		);

		const sizeWatch = new ResizeObserver(() => {
			width = host.clientWidth;
			plot?.setSize({ width: Math.max(width, 1), height });
		});
		sizeWatch.observe(host);

		// A theme switch rewrites custom properties on <html>; re-read them by rebuilding.
		const theme = new MutationObserver(() => create?.());
		theme.observe(document.documentElement, {
			attributes: true,
			attributeFilter: ['data-theme', 'style', 'class']
		});

		return () => {
			disposed = true;
			create = undefined;
			sizeWatch.disconnect();
			theme.disconnect();
			destroy();
		};
	});
</script>

<div
	bind:this={host}
	class="w-full line-chart"
	style:min-height="{height}px"
	role="img"
	aria-label={label}
	data-testid={testid}
	data-ready={ready}
	data-failed={failed}
></div>

<style>
	/* The legend keys series with a line swatch, not a checkbox square, and shows values only
	   while the pointer is over the chart (the data table has them all). */
	.line-chart :global(.u-legend .u-marker) {
		width: calc(var(--spacing) * 4);
		height: calc(var(--spacing) * 0.5);
		border-width: 0;
		border-radius: 0;
		vertical-align: middle;
	}
	.line-chart:not(:hover) :global(.u-legend .u-value) {
		visibility: hidden;
	}
	.line-chart:not(:hover) :global(.u-legend .u-series:first-child) {
		visibility: hidden;
	}
</style>
