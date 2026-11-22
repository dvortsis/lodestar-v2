<script lang="ts">
	// The seeders/leechers line over time (§10.5.4), in-house SVG (no uPlot in the drawer). Gaps are
	// gaps, never zeros; the value axis starts at zero and says its unit; each sample's tooltip has
	// the full date; a table carries the same numbers for assistive tech.
	import DataTableAlternative from '$lib/charts/DataTableAlternative.svelte';
	import { DASH, formatCount, formatDate, formatDateFull } from '$lib/format';
	import { currentLocale, displayTimeZone } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { buildChart, type Sample } from './geometry';

	interface Props {
		samples: Sample[];
		/** Seconds between samples: a day or a week. */
		bucket: number;
	}
	let { samples, bucket }: Props = $props();

	const W = 300;
	const H = 96;
	const locale = $derived(currentLocale());
	const zone = $derived(displayTimeZone());
	const chart = $derived(buildChart(samples, bucket, W, H));
	const unit = $derived(bucket >= 604_800 ? 'week' : 'day');
	const full = (at: number) => formatDateFull(at, locale, { bucket: 'day', timeZone: zone });
	const count = (v: number | null) => formatCount(v, locale);
	const tip = (p: { at: number; seeders: number | null; leechers: number | null }) =>
		`${full(p.at)} · ${m.seeders()} ${p.seeders === null ? DASH : count(p.seeders)} · ${m.leechers()} ${p.leechers === null ? DASH : count(p.leechers)}`;
	const rows = $derived(
		samples.map((s) => ({
			date: full(s.at),
			seeders: s.seeders === null ? DASH : count(s.seeders),
			leechers: s.leechers === null ? DASH : count(s.leechers)
		}))
	);
</script>

{#if chart}
	<figure
		class="ls-detail-stack"
		data-testid="detail-swarm-chart"
		data-points={chart.points.length}
	>
		<div class="ls-detail-row text-small text-muted-text">
			<span class="inline-flex items-center gap-1" data-testid="detail-swarm-legend-seeders"
				><svg class="h-2 w-5" viewBox="0 0 20 4" aria-hidden="true"
					><path d="M0 2h20" stroke="var(--data-1)" stroke-width="2" fill="none" /></svg
				>{m.seeders()}</span
			>
			<span class="inline-flex items-center gap-1" data-testid="detail-swarm-legend-leechers"
				><svg class="h-2 w-5" viewBox="0 0 20 4" aria-hidden="true"
					><path
						d="M0 2h20"
						stroke="var(--data-2)"
						stroke-width="2"
						stroke-dasharray="4 3"
						fill="none"
					/></svg
				>{m.leechers()}</span
			>
			<span>{m.detail_swarm_unit({ unit: m.detail_swarm_peers() })}</span>
			<span data-testid="detail-swarm-resolution"
				>{unit === 'week' ? m.detail_swarm_weekly() : m.detail_swarm_daily()}</span
			>
		</div>
		<div class="ls-detail-chart">
			<span
				class="ls-detail-chart-max tabular-nums text-small text-muted-text"
				data-testid="detail-swarm-max">{count(chart.max)}</span
			>
			<svg
				viewBox="0 0 {W} {H}"
				preserveAspectRatio="none"
				role="img"
				aria-label={m.detail_swarm_chart_label()}
				class="h-(--ls-chart-height) w-full overflow-visible border-b border-s border-base-300"
			>
				{#each chart.lines as line (line.key)}
					{@const color = line.key === 'seeders' ? 'var(--data-1)' : 'var(--data-2)'}
					{#if line.d}
						<path
							d={line.d}
							fill="none"
							stroke={color}
							stroke-width="1.5"
							stroke-dasharray={line.key === 'leechers' ? '4 3' : undefined}
							stroke-linejoin="round"
							vector-effect="non-scaling-stroke"
						/>
					{/if}
					{#each line.dots as dot, i (i)}<path
							d="M{dot.x} {dot.y}h0"
							stroke={color}
							stroke-width="4"
							stroke-linecap="round"
							vector-effect="non-scaling-stroke"
						/>{/each}
				{/each}
				{#each chart.points as p, i (i)}
					<!-- A wide invisible target per sample: the native tooltip carries the full date. -->
					<rect
						x={p.x - 2}
						y="0"
						width="4"
						height={H}
						fill="transparent"
						data-testid="detail-swarm-point"
					>
						<title>{tip(p)}</title>
					</rect>
				{/each}
			</svg>
			<span class="ls-detail-chart-zero tabular-nums text-small text-muted-text">0</span>
		</div>
		<div class="ls-detail-row justify-between text-small text-muted-text tabular-nums">
			<span data-testid="detail-swarm-from"
				>{formatDate(chart.from, locale, { timeZone: zone })}</span
			>
			<span data-testid="detail-swarm-to">{formatDate(chart.to, locale, { timeZone: zone })}</span>
		</div>
		<DataTableAlternative
			caption={m.detail_swarm_chart_label()}
			columns={[
				{ key: 'date', label: m.detail_swarm_col_date() },
				{ key: 'seeders', label: m.seeders(), numeric: true },
				{ key: 'leechers', label: m.leechers(), numeric: true }
			]}
			{rows}
			testid="detail-swarm-table-alt"
		/>
	</figure>
{/if}
