<script lang="ts">
	// Level 2, Movement (§10.9.2): two separate, aligned line charts on the same times, never a
	// dual axis. A null point is a gap (paused or not running), never 0. With Compare on, the
	// previous period is a muted series. Time is shown in the server's `range.tz` (buckets are UTC).
	import LineChart from '$lib/charts/LineChart.svelte';
	import { formatCompact, formatCount, formatDateFull, type MetricValue } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import ChartPanel from './ChartPanel.svelte';
	import { resolve } from '$app/paths';
	import type { Route } from './href';
	import { csvHref } from './links';
	import type { Overview } from './model';
	import type { InsightsQuery } from './query';

	interface Props {
		overview: Overview;
		query: InsightsQuery;
		/** The range as a phrase for summaries ("the last 7 days"). */
		rangeText: string;
	}
	let { overview, query, rangeText }: Props = $props();

	const locale = $derived(currentLocale());
	const bucket = $derived(overview.range.bucket);
	const tz = $derived(overview.range.tz);
	const times = $derived(overview.trend.map((p) => p.at));
	const unit = $derived(
		bucket === 'hour' ? m.insights_overview_per_hour() : m.insights_overview_per_day()
	);
	const prev = $derived.by(() => {
		const p = overview.previous?.trend;
		if (!p) return undefined;
		if (p.length !== overview.trend.length) {
			if (import.meta.env.DEV)
				console.warn('insights: previous trend length differs; comparison dropped');
			return undefined;
		}
		return p;
	});
	let failed = $state(false);

	function series(field: 'searchable' | 'harvested', label: string) {
		const own = { label, values: overview.trend.map((p) => p[field]), token: '--data-1' };
		return prev
			? [
					own,
					{
						label: m.insights_overview_previous_period(),
						values: prev.map((p) => p[field]),
						token: '--color-muted-text'
					}
				]
			: [own];
	}
	const fmt = (v: number | null | undefined) => formatCount(v, locale);
	// Harvested counts are always approximate (metrics.md §6.3).
	const approxCount = (v: MetricValue, l?: string) =>
		formatCount({ value: typeof v === 'object' && v !== null ? v.value : v, approx: true }, l);
	function rows(field: 'searchable' | 'harvested') {
		return overview.trend.map((p, i) => ({
			at: formatDateFull(p.at, locale, { timeZone: tz, bucket }),
			value: field === 'harvested' ? approxCount(p[field], locale) : fmt(p[field]),
			previous:
				field === 'harvested' ? approxCount(prev?.[i]?.[field], locale) : fmt(prev?.[i]?.[field])
		}));
	}
	const columns = (label: string) => [
		{ key: 'at', label: m.insights_overview_col_time() },
		{ key: 'value', label, numeric: true },
		...(prev
			? [{ key: 'previous', label: m.insights_overview_previous_period(), numeric: true }]
			: [])
	];

	const kindText: Record<string, (text: string) => string> = {
		restart: (t) => m.insights_overview_note_restart({ version: t }),
		stopped: () => m.insights_overview_note_stopped(),
		paused: () => m.insights_overview_note_paused(),
		resumed: () => m.insights_overview_note_resumed(),
		intensity: (t) => m.insights_overview_note_intensity({ level: t })
	};
	const note = (kind: string, text: string) =>
		kindText[kind]?.(text) ?? m.insights_overview_note_unknown({ kind });
</script>

<div class="ins-col" data-testid="insights-movement-charts">
	{#each [{ id: 'new', field: 'searchable' as const, title: m.insights_overview_chart_new(), csv: 'trend' }, { id: 'harvested', field: 'harvested' as const, title: m.insights_overview_chart_harvested(), csv: undefined }] as c (c.id)}
		<ChartPanel
			id="ins-{c.id}"
			title={c.title}
			summary={m.insights_overview_chart_summary({ title: c.title, unit, range: rangeText })}
			caption={c.title}
			columns={columns(c.title)}
			rows={rows(c.field)}
			csv={c.csv}
			{query}
			forceTable={failed}
			testid="insights-chart-{c.id}"
		>
			<LineChart
				{times}
				series={series(c.field, c.title)}
				label={m.insights_overview_chart_summary({ title: c.title, unit, range: rangeText })}
				{unit}
				format={c.field === 'harvested' ? approxCount : formatCount}
				axisFormat={formatCompact}
				{bucket}
				timeZone={tz}
				timeLabel={m.insights_overview_col_time()}
				onerror={() => (failed = true)}
				testid="insights-line-{c.id}"
			/>
		</ChartPanel>
	{/each}
</div>

{#if overview.annotations.length}
	<div class="ins-card" data-testid="insights-annotations">
		<div class="ins-card-head">
			<h3 class="ins-card-title">{m.insights_overview_annotations()}</h3>
			<a
				class="ins-link"
				href={resolve(csvHref('annotations', query) as Route)}
				download
				data-testid="insights-annotations-csv">{m.insights_overview_csv()}</a
			>
		</div>
		<ol class="ins-list">
			{#each overview.annotations as a, i (a.at + a.kind + i)}
				<li class="ins-item-row" data-testid="insights-annotation-{a.kind}">
					<span>{note(a.kind, a.text)}</span>
					<span class="ins-muted ins-num">{formatDateFull(a.at, locale, { timeZone: tz })}</span>
				</li>
			{/each}
		</ol>
	</div>
{/if}
