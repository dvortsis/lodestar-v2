<script lang="ts">
	// The Insights overview (§10.9): one story, top to bottom. This file only composes: the data and
	// the crawler controls live in `state.svelte.ts`, the URL contract in `query.ts`.
	import { untrack } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { formatCount, formatDate } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import ContextBar from './ContextBar.svelte';
	import DetailLinks from './DetailLinks.svelte';
	import Exceptions from './Exceptions.svelte';
	import Funnel from './Funnel.svelte';
	import Hatch from './Hatch.svelte';
	import { resolve } from '$app/paths';
	import type { Route } from './href';
	import KpiRow from './KpiRow.svelte';
	import Movement from './Movement.svelte';
	import { csvHref } from './links';
	import { needsCanonical, parseQuery, searchFor, type InsightsQuery } from './query';
	import Segments from './Segments.svelte';
	import ServePanel from './ServePanel.svelte';
	import Skeleton from './Skeleton.svelte';
	import StageStrip from './StageStrip.svelte';
	import { createInsights } from './state.svelte';
	import './insights.css';

	const insights = createInsights();
	const locale = $derived(currentLocale());
	const params = $derived(page.url.searchParams);
	const query = $derived(parseQuery(params));
	const overview = $derived(insights.overview);

	// The URL is the state: a malformed one is replaced by what it parses to, with no error.
	$effect(() => {
		if (needsCanonical(params)) {
			void goto(resolve((page.url.pathname + searchFor(params, query)) as Route), {
				replaceState: true,
				keepFocus: true,
				noScroll: true
			});
		}
	});
	// One request per state change; Back and Forward land here too.
	const queryKey = $derived(JSON.stringify(query));
	$effect(() => {
		const q = queryKey;
		untrack(() => void insights.load(JSON.parse(q) as InsightsQuery));
	});
	$effect(() => insights.start());

	function setQuery(next: InsightsQuery) {
		void goto(resolve((page.url.pathname + searchFor(params, next)) as Route), {
			keepFocus: true,
			noScroll: true
		});
	}

	function jump(id: string) {
		const heading = document.getElementById(`insights-${id}-heading`);
		heading?.scrollIntoView({ block: 'start' });
		heading?.focus({ preventScroll: true });
	}

	// Palette-only commands (no keys); the registry loads on demand like the shell's.
	$effect(() => {
		let off: (() => void)[] = [];
		let gone = false;
		void import('$lib/keys').then(({ keys }) => {
			if (gone) return;
			const section = () => m.shell_nav_insights();
			off = [
				keys.register({
					id: 'insights.pause',
					title: () => m.insights_overview_pause(),
					section,
					keys: [],
					when: () => insights.crawler?.paused === false,
					run: () => void insights.pause()
				}),
				keys.register({
					id: 'insights.resume',
					title: () => m.insights_overview_resume(),
					section,
					keys: [],
					when: () => insights.crawler?.paused === true,
					run: () => void insights.resume()
				}),
				keys.register({
					id: 'insights.compare',
					title: () => m.insights_overview_compare(),
					section,
					keys: [],
					when: () => query.range !== 'all',
					run: () => setQuery({ ...query, compare: !query.compare })
				})
			];
		});
		return () => {
			gone = true;
			off.forEach((f) => f());
		};
	});

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
	const fieldError = $derived(
		insights.error?.status === 400 &&
			(insights.error.field === 'from' || insights.error.field === 'to')
			? { field: insights.error.field, message: insights.error.message }
			: undefined
	);
	const notices = {
		paused: () => m.insights_overview_notice_paused(),
		resumed: () => m.insights_overview_notice_resumed()
	};
	const announcement = $derived(insights.notice ? notices[insights.notice]() : '');
	const paused = $derived(overview?.kpi.health.state === 'paused');
	const staleText: Record<string, () => string> = {
		paused: () => m.insights_overview_stale_paused(),
		writer_stalled: () => m.insights_overview_stale_writer()
	};
	const staleReason = $derived(
		overview?.stale
			? (staleText[overview.stale.reason]?.() ??
					m.insights_overview_stale_other({ reason: overview.stale.reason }))
			: ''
	);

	// The scenes are art: they load on demand and never join the first-load chunk.
	let Scene = $state<typeof import('$lib/shell/StateScene.svelte').default>();
	$effect(() => {
		if (paused || insights.startup === 'yes')
			void import('$lib/shell/StateScene.svelte').then((mod) => (Scene = mod.default));
	});
</script>

<Hatch />
<div class="ins-page" data-testid="insights-page">
	<h1 class="sr-only">{m.insights_overview_title()}</h1>

	<ContextBar
		{query}
		tz={overview?.range.tz ?? 'UTC'}
		updatedAt={overview?.updated_at}
		crawler={insights.crawler}
		busy={insights.busy}
		crawlerError={insights.crawlerError}
		{fieldError}
		onrange={(range) => setQuery({ range, compare: query.compare })}
		oncustom={(from, to) => setQuery({ range: 'custom', from, to, compare: query.compare })}
		oncompare={(compare) => setQuery({ ...query, compare })}
		onpause={() => void insights.pause()}
		onresume={() => void insights.resume()}
		onintensity={(level) => void insights.setIntensity(level)}
	/>

	<div class="sr-only" role="status" aria-live="polite" data-testid="insights-announce">
		{announcement}
	</div>

	{#if insights.phase === 'error' && fieldError}
		<p class="ins-sentence" data-testid="insights-range-invalid">
			{m.insights_overview_range_invalid()}
		</p>
	{:else if insights.phase === 'error'}
		<Banner kind="error" title={m.insights_overview_error_title()}>
			{m.insights_overview_error_body()}
			{#snippet action()}
				<Button onclick={() => void insights.retry()} data-testid="insights-retry"
					>{m.insights_overview_retry()}</Button
				>
			{/snippet}
		</Banner>
	{:else if !overview}
		<Skeleton />
	{:else}
		<StageStrip stages={overview.stages} onjump={jump} />

		{#if overview.stale}
			<Banner kind="warning" title={m.insights_overview_stale_title()}>
				<span data-testid="insights-stale">{staleReason}</span>
				{m.insights_overview_stale_since()}
				<RelativeTime seconds={overview.stale.since} timeZone={overview.range.tz} />.
			</Banner>
		{/if}

		{#if paused}
			<section
				class="ins-card ins-scene"
				aria-label={m.insights_overview_paused_title()}
				data-testid="insights-paused"
			>
				<div>
					<h2 class="ins-heading">{m.insights_overview_paused_title()}</h2>
					<p class="ins-sentence">{m.insights_overview_paused_body()}</p>
				</div>
				<Button
					variant="primary"
					loading={insights.busy === 'resume'}
					onclick={() => void insights.resume()}
					data-testid="insights-paused-resume"
				>
					{m.insights_overview_resume()}
				</Button>
				<div class="ins-scene-art">
					{#if Scene}<Scene id="offline_idle" />{/if}
				</div>
			</section>
		{:else if insights.startup === 'yes'}
			<section
				class="ins-card ins-scene"
				aria-label={m.insights_overview_starting_title()}
				data-testid="insights-starting"
			>
				<div>
					<h2 class="ins-heading">{m.insights_overview_starting_title()}</h2>
					<p class="ins-sentence">{m.insights_overview_starting_body()}</p>
				</div>
				<dl class="ins-counters">
					<div>
						<dt>{m.harvested()}</dt>
						<dd data-testid="insights-starting-harvested">
							{formatCount(insights.live?.harvested, locale)}
						</dd>
					</div>
					<div>
						<dt>{m.insights_overview_funnel_fetched()}</dt>
						<dd data-testid="insights-starting-fetched">
							{formatCount(insights.live?.fetched, locale)}
						</dd>
					</div>
					<div>
						<dt>{m.insights_overview_funnel_stored()}</dt>
						<dd data-testid="insights-starting-stored">
							{formatCount(insights.live?.stored, locale)}
						</dd>
					</div>
				</dl>
				<div class="ins-scene-art">
					{#if Scene}<Scene id="searching_crawling" />{/if}
				</div>
			</section>
		{/if}

		<section id="insights-status" class="ins-section" aria-labelledby="insights-status-heading">
			<h2 id="insights-status-heading" class="ins-heading" tabindex="-1">
				{m.insights_overview_status_title()}
			</h2>
			<p class="ins-sub">{m.insights_overview_status_time()}</p>
			<KpiRow kpi={overview.kpi} />
		</section>

		<div class="ins-grid">
			<section
				id="insights-movement"
				class="ins-section ins-movement"
				aria-labelledby="insights-movement-heading"
			>
				<h2 id="insights-movement-heading" class="ins-heading" tabindex="-1">
					{m.insights_overview_movement_title()}
				</h2>
				<p class="ins-sub">{m.insights_overview_movement_time({ tz: overview.range.tz })}</p>
				<Movement {overview} {query} {rangeText} />
			</section>

			<section
				id="insights-exceptions"
				class="ins-section ins-exceptions"
				aria-labelledby="insights-exceptions-heading"
			>
				<h2 id="insights-exceptions-heading" class="ins-heading" tabindex="-1">
					{m.insights_overview_exceptions_title()}
				</h2>
				<p class="ins-sub">{m.insights_overview_exceptions_time()}</p>
				<div class="ins-card">
					<Exceptions exceptions={overview.exceptions} />
					<a
						class="ins-link"
						href={resolve(csvHref('exceptions', query) as Route)}
						download
						data-testid="insights-exceptions-csv">{m.insights_overview_csv()}</a
					>
				</div>
			</section>

			<section
				id="insights-funnel"
				class="ins-section ins-funnel"
				aria-labelledby="insights-funnel-heading"
			>
				<h2 id="insights-funnel-heading" class="ins-heading" tabindex="-1">
					{m.insights_overview_funnel_heading()}
				</h2>
				<p class="ins-sub">{m.insights_overview_funnel_time()}</p>
				<Funnel {overview} {query} {rangeText} />
			</section>
		</div>

		<section id="insights-segments" class="ins-section" aria-labelledby="insights-segments-heading">
			<h2 id="insights-segments-heading" class="ins-heading" tabindex="-1">
				{m.insights_overview_segments_title()}
			</h2>
			<p class="ins-sub">{m.insights_overview_segments_time()}</p>
			<Segments segments={overview.segments} {query} {rangeText} />
		</section>

		<section id="insights-serve" class="ins-section" aria-labelledby="insights-serve-heading">
			<h2 id="insights-serve-heading" class="ins-heading" tabindex="-1">
				{m.insights_overview_serve_title()}
			</h2>
			<p class="ins-sub">{m.insights_overview_serve_time()}</p>
			<ServePanel serve={overview.serve} {query} />
		</section>

		<section id="insights-detail" class="ins-section" aria-labelledby="insights-detail-heading">
			<h2 id="insights-detail-heading" class="ins-heading" tabindex="-1">
				{m.insights_overview_detail_title()}
			</h2>
			<DetailLinks {query} />
		</section>
	{/if}
</div>
