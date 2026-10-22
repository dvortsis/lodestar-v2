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
	import { currentLocale } from '$lib/i18n/locale.svelte';
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
	const fmt = $derived<LineFormat>({
		axis: (v) => axisFormat(v as MetricValue, locale),
		y: (v) => format(v as MetricValue, locale),
		tick: (s) =>
			bucket === 'hour'
				? formatDateTime(s, locale, { timeZone })
				: formatDate(s, locale, { timeZone }),
		full: (s) => formatDateFull(s, locale, { timeZone, bucket })
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

		const resize = new ResizeObserver(() => {
			width = host.clientWidth;
			plot?.setSize({ width: Math.max(width, 1), height });
		});
		resize.observe(host);

		// A theme switch rewrites custom properties on <html>; re-read them by rebuilding.
		const theme = new MutationObserver(() => create?.());
		theme.observe(document.documentElement, {
			attributes: true,
			attributeFilter: ['data-theme', 'style', 'class']
		});

		return () => {
			disposed = true;
			create = undefined;
			resize.disconnect();
			theme.disconnect();
			destroy();
		};
	});
</script>

<div
	bind:this={host}
	class="w-full"
	style:min-height="{height}px"
	role="img"
	aria-label={label}
	data-testid={testid}
	data-ready={ready}
	data-failed={failed}
></div>
