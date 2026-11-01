<script lang="ts">
	// A stage detail page (§10.9.5): breadcrumb and stage strip, 3–4 KPIs, one primary trend, then
	// drivers and tables. One component for the four stages that have a typed endpoint; the data
	// mapping is in `model.ts`. The range and Compare come from the URL and are kept by every link.
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { api, isAbort, latest } from '$lib/api/client';
	import DataTableAlternative from '$lib/charts/DataTableAlternative.svelte';
	import LineChart from '$lib/charts/LineChart.svelte';
	import Histogram from '$lib/charts/Histogram.svelte';
	import {
		formatCompact,
		formatCount,
		formatDateFull,
		formatDate,
		type MetricValue
	} from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import SegmentedControl from '$lib/ui/controls/SegmentedControl.svelte';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import ChartPanel from '../overview/ChartPanel.svelte';
	import type { Route } from '../overview/href';
	import { csvHref, searchHref, stageHref, STAGES, type Stage } from '../overview/links';
	import ImportMarkers from '../overview/ImportMarkers.svelte';
	import type { Annotation } from '../overview/imports';
	import LinkedBars from '../overview/LinkedBars.svelte';
	import {
		apiQuery,
		parseQuery,
		RANGES,
		searchFor,
		type InsightsQuery,
		type RangeId
	} from '../overview/query';
	import Skeleton from '../overview/Skeleton.svelte';
	import '../overview/insights.css';
	import {
		binEdges,
		discoverBars,
		discoverTables,
		binLabel,
		edgeLabels,
		fetchBars,
		formatMetric,
		indexBars,
		mapStage,
		trackerTable,
		understandBars,
		type BarsSpec,
		type Metric,
		type StageId,
		type StageModel,
		type StageResponse,
		type Table
	} from './model';

	let { stage }: { stage: StageId } = $props();

	const locale = $derived(currentLocale());
	const params = $derived(page.url.searchParams);
	const query = $derived(parseQuery(params));
	// Only a known loss name is sent; anything else is ignored, not a 400 the user cannot fix.
	const reason = $derived.by(() => {
		const r = stage === 'fetch' ? params.get('reason') : null;
		return r && r in reasons ? r : undefined;
	});

	let data = $state.raw<StageResponse>();
	let phase = $state<'loading' | 'ready' | 'error'>('loading');
	let failedChart = $state(false);
	const slot = latest();

	async function load(q: InsightsQuery, r: string | undefined) {
		phase = 'loading';
		data = undefined;
		failedChart = false;
		const base = { ...apiQuery(q), reason: r };
		try {
			const one = (signal: AbortSignal): Promise<StageResponse> =>
				stage === 'discover'
					? api.get('/insights/discover', { query: base, signal })
					: stage === 'fetch'
						? api.get('/insights/fetch', { query: base, signal })
						: stage === 'understand'
							? api.get('/insights/understand', { query: base, signal })
							: stage === 'index'
								? api.get('/insights/index', { query: base, signal })
								: api.get('/insights/serve', { query: base, signal });
			data = await slot.run(one);
			phase = 'ready';
		} catch (e) {
			if (isAbort(e)) return;
			phase = 'error';
		}
	}
	// The Index page explains a removal spike with the import markers in its range. They load after
	// the page and are best-effort: no markers is the page as it was.
	let marks = $state.raw<Annotation[]>([]);
	$effect(() => {
		const d = data;
		marks = [];
		if (stage !== 'index' || !d || phase !== 'ready') return;
		const ctl = new AbortController();
		api
			.get('/insights/events', {
				query: { from: d.range.from, to: d.range.to },
				signal: ctl.signal
			})
			.then((r) => (marks = r.annotations))
			.catch(() => {});
		return () => ctl.abort();
	});
	const key = $derived(JSON.stringify([stage, query, reason]));
	$effect(() => {
		const [, q, r] = JSON.parse(key) as [string, InsightsQuery, string | undefined];
		void load(q, r);
		return () => slot.cancel();
	});

	const names: Record<Stage, () => string> = {
		discover: () => m.insights_stage_discover(),
		fetch: () => m.insights_stage_fetch(),
		understand: () => m.insights_stage_understand(),
		index: () => m.insights_stage_index(),
		serve: () => m.insights_stage_serve()
	};
	// Explicit maps: a lookup by built key (`m[...]`) would keep every message in the bundle.
	const reasons: Record<string, () => string> = {
		no_peers: () => m.insights_loss_no_peers(),
		timed_out: () => m.insights_loss_timed_out(),
		refused: () => m.insights_loss_refused(),
		encryption_failed: () => m.insights_loss_encryption_failed(),
		oversize: () => m.insights_loss_oversize(),
		invalid: () => m.insights_loss_invalid(),
		blocked: () => m.insights_loss_blocked(),
		evicted: () => m.insights_loss_evicted(),
		gave_up: () => m.insights_loss_gave_up(),
		unreachable: () => m.insights_loss_unreachable()
	};
	const groups: Record<string, () => string> = {
		video: () => m.insights_group_video(),
		audio: () => m.insights_group_audio(),
		documents: () => m.insights_group_documents(),
		software: () => m.insights_group_software(),
		images: () => m.insights_group_images(),
		other: () => m.insights_group_other(),
		adult: () => m.insights_group_adult(),
		unclassified: () => m.insights_group_unclassified()
	};
	const types: Record<string, () => string> = {
		movie: () => m.insights_type_movie(),
		tv: () => m.insights_type_tv(),
		video: () => m.insights_type_video(),
		music: () => m.insights_type_music(),
		audiobook: () => m.insights_type_audiobook(),
		ebook: () => m.insights_type_ebook(),
		comic: () => m.insights_type_comic(),
		software: () => m.insights_type_software(),
		game: () => m.insights_type_game(),
		images: () => m.insights_type_images(),
		other: () => m.insights_type_other(),
		adult: () => m.insights_type_adult()
	};
	const reasonName = (k: string) => reasons[k]?.() ?? k;
	const groupName = (k: string) => groups[k]?.() ?? k;
	const typeName = (k: string) => types[k]?.() ?? k;

	const model = $derived<StageModel | undefined>(data ? mapStage(stage, data, locale) : undefined);
	const extraBars = $derived.by<BarsSpec[]>(() => {
		if (!data) return [];
		if (stage === 'discover') return discoverBars(data as never, locale);
		if (stage === 'fetch') return fetchBars(data as never, reasonName, locale);
		if (stage === 'understand') return understandBars(data as never, groupName, typeName, locale);
		if (stage === 'index') return [indexBars(data as never, typeName, locale)];
		return [];
	});
	const extraTables = $derived.by<Table[]>(() =>
		!data
			? []
			: stage === 'index'
				? [trackerTable(data as never, locale)]
				: stage === 'discover'
					? discoverTables(data as never, locale)
					: []
	);
	const rangeText = $derived.by(() => {
		if (query.range === 'custom' && query.from !== undefined && query.to !== undefined) {
			return m.insights_overview_text_custom({
				from: formatDate(query.from, locale, { timeZone: 'UTC' }),
				to: formatDate(query.to - 1, locale, { timeZone: 'UTC' })
			});
		}
		return {
			'24h': m.insights_overview_text_24h(),
			'7d': m.insights_overview_text_7d(),
			'30d': m.insights_overview_text_30d(),
			all: m.insights_overview_text_all(),
			custom: m.insights_overview_text_7d()
		}[query.range];
	});
	const rangeNames: Record<string, () => string> = {
		'24h': () => m.insights_overview_range_24h(),
		'7d': () => m.insights_overview_range_7d(),
		'30d': () => m.insights_overview_range_30d(),
		all: () => m.insights_overview_range_all(),
		custom: () => m.insights_overview_range_custom()
	};
	const bucket = $derived(data?.range.bucket ?? 'hour');
	const tz = $derived(data?.range.tz ?? 'UTC');
	const unit = $derived(
		bucket === 'hour' ? m.insights_overview_per_hour() : m.insights_overview_per_day()
	);
	const withContext = (path: string, q: InsightsQuery, keep?: string) => {
		const s = searchFor(new URLSearchParams(), q);
		if (!keep) return path + s;
		return `${path}${s}${s ? '&' : '?'}reason=${encodeURIComponent(keep)}`;
	};
	const ranges = $derived(
		RANGES.filter((r) => r !== 'custom' || query.range === 'custom').map((r) => ({
			value: r,
			label: rangeNames[r]?.() ?? r
		}))
	);
	const rangeLink = (r: RangeId) =>
		withContext(
			`/insights/${stage}`,
			{
				...query,
				range: r,
				compare: r !== 'all' && query.compare,
				from: query.from,
				to: query.to
			},
			reason
		);
	const compareLink = $derived(
		withContext(`/insights/${stage}`, { ...query, compare: !query.compare }, reason)
	);
	const overviewHref = $derived(withContext('/insights', query));
	const stageLink = (s: Stage) => stageHref(s, query, { compare: true, reason: undefined });
	const staleReason: Record<string, () => string> = {
		paused: () => m.insights_overview_stale_paused(),
		writer_stalled: () => m.insights_overview_stale_writer()
	};
	const csv = (panel: string) =>
		csvHref(panel, query) + (reason ? `&reason=${encodeURIComponent(reason)}` : '');
	const hrefFor = (barId: string, key: string) => {
		if (stage === 'fetch') {
			return reason ? undefined : stageHref('fetch', query, { compare: true, reason: key });
		}
		if (stage === 'understand') {
			return searchHref(barId === 'by-group' ? 'group' : 'type', key, query);
		}
		return undefined;
	};
	const approxCount = (v: MetricValue, l?: string) =>
		formatCount({ value: typeof v === 'object' && v !== null ? v.value : v, approx: true }, l);
	const fmtPoint = (approx: boolean | undefined, v: number | null | undefined) =>
		approx ? approxCount(v ?? null, locale) : formatCount(v, locale);
	const previousLabel = (label: string) => `${label} · ${m.insights_overview_previous_period()}`;
	// A snapshot chart is readings at the end of each bucket, so it has no "per hour" unit.
	const summaryOf = (c: { title: string; snapshot?: boolean }) =>
		c.snapshot
			? m.insights_stage_chart_summary_snapshot({ title: c.title, range: rangeText })
			: m.insights_overview_chart_summary({ title: c.title, unit, range: rangeText });
	const noData = (series: { values: (number | null)[] }[]) =>
		series.every((s) => s.values.every((v) => v === null));

	const metricText = (x: Metric, v: number | null | undefined) => formatMetric(x, v, locale);
	const hasPrev = (x: Metric) => query.compare && x.previous !== undefined;
</script>

{#snippet metricValue(x: Metric)}
	<span class="ins-num" data-testid="insights-stage-{x.key}-value">{metricText(x, x.value)}</span>
	{#if x.value === null}<span class="sr-only">{m.insights_stage_na()}</span>{/if}
{/snippet}

<div class="ins-page" data-testid="insights-stage-page">
	<nav aria-label={m.insights_stage_breadcrumb()} data-testid="insights-breadcrumb">
		<ol class="ins-row">
			<li>
				<a class="ins-link" href={resolve(overviewHref as Route)} data-testid="insights-back"
					>{m.insights_overview_title()}</a
				>
			</li>
			<li><span class="ins-muted">/</span> <span aria-current="page">{names[stage]()}</span></li>
		</ol>
	</nav>
	<h1 class="ins-heading" tabindex="-1">{names[stage]()}</h1>

	<nav aria-label={m.insights_overview_strip_label()} data-testid="insights-strip">
		<ol class="ins-row">
			{#each STAGES as s (s)}
				<li>
					<a
						class="ins-link"
						href={resolve(stageLink(s) as Route)}
						aria-current={s === stage ? 'page' : undefined}
						data-testid="insights-stage-{s}">{names[s]()}</a
					>
				</li>
			{/each}
		</ol>
	</nav>

	<div class="ins-row" data-testid="insights-stage-context">
		<SegmentedControl
			options={ranges}
			value={query.range}
			label={m.insights_overview_range_label()}
			onchange={(r) => void goto(resolve(rangeLink(r as RangeId) as Route))}
		/>
		{#if query.range !== 'all'}
			<Button
				variant={query.compare ? 'primary' : 'secondary'}
				dense
				aria-pressed={query.compare}
				onclick={() => void goto(resolve(compareLink as Route))}
				data-testid="insights-stage-compare"
			>
				{m.insights_overview_compare()}
			</Button>
		{/if}
		{#if reason}
			<a
				class="ins-link"
				href={resolve(withContext(`/insights/${stage}`, query) as Route)}
				data-testid="insights-reason-chip"
				>{m.insights_stage_reason_reset({ reason: reasonName(reason) })}</a
			>
		{/if}
		{#if data}
			<span class="ins-muted ins-grow" data-testid="insights-stage-updated">
				{m.insights_overview_stale_since()}
				<RelativeTime seconds={data.updated_at} timeZone={tz} /> · {m.insights_overview_tz({ tz })}
			</span>
		{/if}
	</div>

	{#if phase === 'error'}
		<Banner kind="error" title={m.insights_overview_error_title()}>
			{m.insights_stage_error_body()}
			{#snippet action()}
				<Button onclick={() => void load(query, reason)} data-testid="insights-retry"
					>{m.insights_overview_retry()}</Button
				>
			{/snippet}
		</Banner>
	{:else if !model || !data}
		<Skeleton />
	{:else}
		{#if data.stale}
			<Banner kind="warning" title={m.insights_overview_stale_title()}>
				<span data-testid="insights-stale"
					>{staleReason[data.stale.reason]?.() ??
						m.insights_overview_stale_other({ reason: data.stale.reason })}</span
				>
				{m.insights_overview_stale_since()}
				<RelativeTime seconds={data.stale.since} timeZone={tz} />.
			</Banner>
		{/if}

		<section class="ins-section" aria-labelledby="ins-s-kpi">
			<h2 id="ins-s-kpi" class="ins-heading">{m.insights_stage_kpis()}</h2>
			<p class="ins-sub">{m.insights_stage_time_kpis({ tz })}</p>
			<div class="ins-kpis" data-testid="insights-stage-kpis">
				{#each model.kpis as x (x.key)}
					<div class="ins-card" data-testid="insights-stage-kpi-{x.key}">
						<span class="ins-muted">{x.label}</span>
						<span class="ins-status">{@render metricValue(x)}</span>
						{#if x.snapshot}
							<span class="ins-kpi-note" data-testid="insights-stage-{x.key}-snapshot"
								>{m.insights_stage_snapshot()}</span
							>
						{/if}
						{#if hasPrev(x)}
							<span class="ins-kpi-note"
								>{m.insights_overview_previous_period()}:
								<span data-testid="insights-stage-{x.key}-previous"
									>{metricText(x, x.previous)}</span
								></span
							>
						{/if}
						{#if x.value === null}
							<span class="ins-kpi-note" data-testid="insights-stage-{x.key}-na"
								>{m.insights_stage_na()}</span
							>
						{/if}
					</div>
				{/each}
			</div>
		</section>

		{#each model.charts as c (c.id)}
			{@const times = data.trend.map((p) => p.at)}
			<section class="ins-section" aria-labelledby="ins-{c.id}-h">
				<h2 id="ins-{c.id}-h" class="ins-heading">{c.title}</h2>
				<p class="ins-sub">
					{c.snapshot
						? m.insights_stage_time_snapshot({ tz })
						: m.insights_stage_time_event({ tz })}
				</p>
				{#if noData(c.series)}
					<p class="ins-sentence" data-testid="insights-chart-{c.id}-empty">
						{m.insights_stage_nothing_measured()}
					</p>
				{:else}
					<ChartPanel
						id="ins-{c.id}"
						title={c.title}
						summary={summaryOf(c)}
						caption={c.title}
						columns={[
							{ key: 'at', label: m.insights_overview_col_time() },
							...c.series.flatMap((s) => [
								{ key: s.key, label: s.label, numeric: true },
								...(query.compare && s.previous
									? [{ key: `${s.key}-prev`, label: previousLabel(s.label), numeric: true }]
									: [])
							])
						]}
						rows={data.trend.map((p, i) => ({
							at: formatDateFull(p.at, locale, { timeZone: tz, bucket }),
							...Object.fromEntries(
								c.series.flatMap((s) => [
									[s.key, fmtPoint(c.approx, s.values[i])],
									...(query.compare && s.previous
										? [[`${s.key}-prev`, fmtPoint(c.approx, s.previous[i])]]
										: [])
								])
							)
						}))}
						csv={reason ? undefined : c.csv}
						{query}
						forceTable={failedChart}
						testid="insights-chart-{c.id}"
					>
						<LineChart
							{times}
							series={c.series.flatMap((s, i) => [
								{ label: s.label, values: s.values, token: `--data-${i + 1}` },
								...(query.compare && s.previous
									? [
											{
												label: previousLabel(s.label),
												values: s.previous,
												token: '--color-muted-text'
											}
										]
									: [])
							])}
							label={summaryOf(c)}
							unit={c.snapshot ? (c.snapshotUnit ?? m.insights_stage_unit_torrents()) : unit}
							format={c.approx ? approxCount : formatCount}
							axisFormat={formatCompact}
							{bucket}
							timeZone={tz}
							timeLabel={m.insights_overview_col_time()}
							onerror={() => (failedChart = true)}
							testid="insights-line-{c.id}"
						/>
					</ChartPanel>
					{#if reason && c.csv}
						<a
							class="ins-link"
							href={resolve(csv(c.csv) as Route)}
							download
							data-testid="insights-chart-{c.id}-csv">{m.insights_overview_csv()}</a
						>
					{/if}
				{/if}
			</section>
		{/each}

		{#if stage === 'index'}<ImportMarkers annotations={marks} {tz} />{/if}

		{#if extraBars.length || model.hists.length}
			<section class="ins-section" aria-labelledby="ins-s-drivers">
				<h2 id="ins-s-drivers" class="ins-heading">{m.insights_stage_drivers()}</h2>
				{#if stage === 'fetch'}<p class="ins-sub">{m.insights_stage_time_event({ tz })}</p>{/if}
				{#each extraBars as b (b.id)}
					<div class="ins-card" data-testid="insights-bars-{b.id}">
						<div class="ins-card-head">
							<h3 class="ins-card-title">{b.title}</h3>
							{#if b.csv}
								<a
									class="ins-link"
									href={resolve(csv(b.csv) as Route)}
									download
									data-testid="insights-bars-{b.id}-csv">{m.insights_overview_csv()}</a
								>
							{/if}
						</div>
						{#if b.unavailable}
							<p class="ins-muted" data-testid="insights-bars-{b.id}-na">{m.insights_stage_na()}</p>
						{:else if b.items.length}
							<LinkedBars
								items={b.items.map((i) => ({
									key: i.key,
									label: i.label,
									value: i.value,
									text: i.text,
									sub: i.sub,
									href: hrefFor(b.id, i.key)
								}))}
								max={Math.max(...b.items.map((i) => i.value))}
								label={b.title}
								testid="insights-list-{b.id}"
							/>
						{:else}
							<p class="ins-muted" data-testid="insights-bars-{b.id}-empty">
								{m.insights_stage_nothing()}
							</p>
						{/if}
					</div>
				{/each}
				{#each model.hists as h (h.id)}
					{@const edges = h.bins ? binEdges(h.bins) : undefined}
					<div data-testid="insights-hist-{h.id}">
						{#if !h.bins}
							<div class="ins-card">
								<h3 class="ins-card-title">{h.title}</h3>
								<p class="ins-muted" data-testid="insights-hist-{h.id}-na">
									{m.insights_stage_na()}
								</p>
							</div>
						{:else}
							<ChartPanel
								id="ins-hist-{h.id}"
								title={h.title}
								summary={m.insights_stage_hist_summary({ title: h.title })}
								caption={h.title}
								columns={[
									{ key: 'bin', label: m.insights_stage_col_range() },
									{ key: 'count', label: m.insights_stage_col_count(), numeric: true }
								]}
								rows={h.bins.map((b) => ({
									bin: binLabel(b, h.fmt, locale),
									count: formatCount(b.count, locale)
								}))}
								{query}
								forceTable={!edges}
								testid="insights-histogram-{h.id}"
							>
								{#if edges}
									<Histogram
										counts={h.bins.map((b) => b.count)}
										edges={edgeLabels(edges, h.fmt, locale)}
										label={h.title}
										testid="insights-histogram-{h.id}-chart"
									/>
								{/if}
							</ChartPanel>
						{/if}
					</div>
				{/each}
			</section>
		{/if}

		<section class="ins-section" aria-labelledby="ins-s-more">
			<h2 id="ins-s-more" class="ins-heading">{m.insights_stage_more()}</h2>
			<div class="ins-card">
				<dl class="ins-list" data-testid="insights-stage-more">
					{#each model.more as x (x.key)}
						<div class="ins-item-row" data-testid="insights-stage-more-{x.key}">
							<dt>{x.label}</dt>
							<dd class="ins-num">
								{@render metricValue(x)}
								{#if x.value === null}
									<span class="ins-muted" data-testid="insights-stage-{x.key}-na"
										>{m.insights_stage_na()}</span
									>
								{:else if hasPrev(x)}
									<span class="ins-muted" data-testid="insights-stage-{x.key}-previous"
										>· {metricText(x, x.previous)}</span
									>
								{/if}
							</dd>
						</div>
					{/each}
					{#each model.notes as n (n.key)}
						<div class="ins-item-row">
							<dt>{n.label}</dt>
							<dd data-testid="insights-stage-note-{n.key}">{n.text}</dd>
						</div>
					{/each}
				</dl>
			</div>
		</section>

		{#each [...model.tables, ...extraTables] as t (t.id)}
			<section class="ins-section" aria-labelledby="ins-t-{t.id}">
				<h2 id="ins-t-{t.id}" class="ins-heading">{t.title}</h2>
				{#if t.csv}
					<a
						class="ins-link"
						href={resolve(csv(t.csv) as Route)}
						download
						data-testid="insights-table-{t.id}-csv">{m.insights_overview_csv()}</a
					>
				{/if}
				{#if t.empty && !t.rows.length}
					<p class="ins-sentence" data-testid="insights-table-{t.id}-na">{t.empty}</p>
				{:else if !t.rows.length}
					<p class="ins-sentence" data-testid="insights-table-{t.id}-empty">
						{m.insights_stage_nothing()}
					</p>
				{:else}
					<DataTableAlternative
						caption={t.title}
						columns={t.columns}
						rows={t.rows}
						visible
						testid="insights-table-{t.id}"
					/>
				{/if}
			</section>
		{/each}
	{/if}
</div>

<style>
	/* The current stage, range and Compare state are shown, not only exposed to assistive tech:
	   weight and underline carry it, not colour. */
	:global(.ins-link[aria-current]) {
		font-weight: var(--font-weight-semibold);
		text-decoration-line: underline;
		text-decoration-thickness: 2px;
	}
</style>
