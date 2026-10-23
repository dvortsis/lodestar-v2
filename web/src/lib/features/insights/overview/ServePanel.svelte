<script lang="ts">
	// Level 6, Serve (§10.9.2): is it useful? Every field is null until the search counters
	// exist, and a null is a dash with "Not measured yet", never 0. The zero-result rate links to
	// the Serve page. Latency is always approximate.
	import Kpi from '$lib/charts/Kpi.svelte';
	import { formatCount, formatLatency, formatPercent, type MetricValue } from '$lib/format';
	import { m } from '$lib/paraglide/messages.js';
	import { resolve } from '$app/paths';
	import type { Route } from './href';
	import { stageHref } from './links';
	import type { Overview } from './model';
	import type { InsightsQuery } from './query';

	interface Props {
		serve: Overview['serve'];
		query: InsightsQuery;
	}
	let { serve, query }: Props = $props();

	const percent = (v: MetricValue, l?: string) => formatPercent(v, l, 1);
</script>

<div class="ins-kpis" data-testid="insights-serve">
	<div class="ins-card">
		<Kpi
			label={m.insights_overview_serve_searches()}
			value={serve.searches}
			format={formatCount}
			testid="insights-serve-searches"
		/>
		{#if serve.searches === null}<span class="ins-kpi-note"
				>{m.insights_overview_not_measured()}</span
			>{/if}
	</div>
	<div class="ins-card">
		<Kpi
			label={m.insights_overview_serve_zero()}
			value={serve.zero_result_rate}
			format={percent}
			testid="insights-serve-zero"
		/>
		{#if serve.zero_result_rate === null}<span class="ins-kpi-note"
				>{m.insights_overview_not_measured()}</span
			>{/if}
		<a
			class="ins-link"
			href={resolve(stageHref('serve', query) as Route)}
			data-testid="insights-serve-zero-link">{m.insights_overview_serve_zero_link()}</a
		>
	</div>
	<div class="ins-card">
		<Kpi
			label={m.insights_overview_serve_latency()}
			value={{ value: serve.latency_p95_ms, approx: true }}
			format={formatLatency}
			testid="insights-serve-latency"
		/>
		{#if serve.latency_p95_ms === null}<span class="ins-kpi-note"
				>{m.insights_overview_not_measured()}</span
			>{/if}
	</div>
	<div class="ins-card">
		<Kpi
			label={m.insights_overview_serve_alerts()}
			value={serve.alerts_fired}
			format={formatCount}
			testid="insights-serve-alerts"
		/>
		{#if serve.alerts_fired === null}<span class="ins-kpi-note"
				>{m.insights_overview_not_measured()}</span
			>{/if}
	</div>
	<div class="ins-card">
		<Kpi
			label={m.insights_overview_serve_sends()}
			value={serve.sends}
			format={formatCount}
			testid="insights-serve-sends"
		/>
		{#if serve.sends === null}<span class="ins-kpi-note">{m.insights_overview_not_measured()}</span
			>{/if}
	</div>
</div>
