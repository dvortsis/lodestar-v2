<script lang="ts">
	import { onMount } from 'svelte';
	import type uPlot from 'uplot';
	import { formatCount, formatDate, formatDateTime, type MetricValue } from '$lib/format';
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
		format?: ValueFormatter;
		/** IANA time zone, the user's setting. */
		timeZone?: string;
		height?: number;
		testid?: string;
	}
	let {
		times,
		series,
		label,
		unit,
		format = formatCount,
		timeZone,
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
		y: (v) => format(v as MetricValue, locale),
		tick: (s) => formatDate(s, locale, { timeZone }),
		full: (s) => formatDateTime(s, locale, { timeZone })
	});

	function destroy() {
		plot?.destroy();
		plot = undefined;
	}

	// Rebuild when data, formatting or the theme changes. Updates redraw in place; uPlot has no
	// animation, so nothing re-animates (motion-ui) and reduced motion needs no special case.
	$effect(() => {
		void [times, series, fmt, unit, height];
		create?.();
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
			() => (failed = true)
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
