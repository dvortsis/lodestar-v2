<script lang="ts">
	// Level 1, Status (§10.9.2): five KPIs. Every number is a response field through its formatter;
	// a null is a dash with its reason, never 0. Deltas show only when the server sent one.
	import IconRunning from '~icons/ph/check-circle';
	import IconPaused from '~icons/ph/pause-circle';
	import IconDegraded from '~icons/ph/warning';
	import Kpi from '$lib/charts/Kpi.svelte';
	import {
		formatCompact,
		formatCount,
		formatPercent,
		formatSpan,
		type MetricValue
	} from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { Overview } from './model';

	let { kpi }: { kpi: Overview['kpi'] } = $props();

	const locale = $derived(currentLocale());
	const vsPrevious = $derived(m.insights_overview_vs_previous());
	const percent = (v: MetricValue, l?: string) => formatPercent(v, l, 1);
	const healthIcon = { running: IconRunning, paused: IconPaused, degraded: IconDegraded };
	const Health = $derived(healthIcon[kpi.health.state]);
	const healthText = $derived(
		{
			running: m.insights_overview_health_running(),
			paused: m.insights_overview_health_paused(),
			degraded: m.insights_overview_health_degraded()
		}[kpi.health.state]
	);
	const reasons: Record<string, () => string> = {
		paused: () => m.insights_overview_health_reason_paused(),
		writer_stalled: () => m.insights_overview_health_reason_writer_stalled(),
		index_lag: () => m.insights_overview_health_reason_index_lag()
	};
	const reason = $derived(
		kpi.health.reason ? (reasons[kpi.health.reason]?.() ?? kpi.health.reason) : undefined
	);
	const delta = (value: number | null) =>
		value === null ? undefined : { value, kind: 'compact' as const, text: vsPrevious };
</script>

<div class="ins-kpis" data-testid="insights-kpis">
	<div class="ins-card">
		<Kpi
			label={m.insights_overview_kpi_searchable()}
			value={kpi.searchable}
			format={formatCompact}
			delta={delta(kpi.searchable_delta)}
			testid="insights-kpi-searchable"
		/>
		<span class="ins-kpi-note"
			>{m.insights_overview_kpi_imported()}
			<span data-testid="insights-kpi-imported">{formatCount(kpi.imported, locale)}</span
			>{#if kpi.imported === null}<span class="sr-only">{m.insights_overview_not_measured()}</span
				>{/if}</span
		>
		{#if kpi.searchable === null}
			<span class="ins-kpi-note">{m.insights_overview_search_off()}</span>
		{/if}
	</div>

	<div class="ins-card">
		<Kpi
			label={m.insights_overview_kpi_new_searchable()}
			value={kpi.new_searchable}
			format={formatCompact}
			delta={delta(kpi.new_searchable_delta)}
			testid="insights-kpi-new"
		/>
	</div>

	<div class="ins-card">
		<Kpi
			label={m.insights_overview_kpi_yield()}
			value={kpi.yield}
			format={percent}
			delta={kpi.yield_delta === null
				? undefined
				: { value: kpi.yield_delta, kind: 'percent', text: vsPrevious }}
			testid="insights-kpi-yield"
		/>
		{#if kpi.yield === null}
			<span class="ins-kpi-note" data-testid="insights-kpi-yield-none"
				>{m.insights_overview_no_completed_cohorts()}</span
			>
		{/if}
	</div>

	<div class="ins-card">
		<Kpi
			label={m.insights_overview_kpi_time()}
			value={{ value: kpi.time_to_searchable_p50_s, approx: true }}
			format={formatSpan}
			testid="insights-kpi-time"
		/>
		<span class="ins-kpi-note"
			>{m.insights_overview_kpi_time_p95()}
			<span data-testid="insights-kpi-time-p95"
				>{formatSpan({ value: kpi.time_to_searchable_p95_s, approx: true }, locale)}</span
			></span
		>
		{#if kpi.time_to_searchable_p50_s === null}
			<span class="ins-kpi-note" data-testid="insights-kpi-time-none"
				>{m.insights_overview_no_completed_cohorts()}</span
			>
		{/if}
	</div>

	<div class="ins-card" data-testid="insights-kpi-health">
		<span class="ins-kpi-note">{m.insights_overview_kpi_health()}</span>
		<span class="ins-status" data-testid="insights-kpi-health-state" data-state={kpi.health.state}>
			<Health class="ins-icon" aria-hidden="true" />{healthText}
		</span>
		{#if reason}<span class="ins-kpi-note" data-testid="insights-kpi-health-reason">{reason}</span
			>{/if}
	</div>
</div>
