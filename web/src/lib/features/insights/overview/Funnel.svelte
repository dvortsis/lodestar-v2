<script lang="ts">
	// Level 3, Drivers (§10.9.2): the funnel by discovery cohort and where torrents were lost.
	// Every figure is a server field: counts, the server's conversion, `final_cohorts`, the loss
	// shares. Bars start at zero and are drawn from counts; incomplete cohorts are hatched.
	import { formatCount, formatDateFull, formatPercent } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import ChartPanel from './ChartPanel.svelte';
	import { resolve } from '$app/paths';
	import type { Route } from './href';
	import { stageHref, type Stage } from './links';
	import LinkedBars, { type BarItem } from './LinkedBars.svelte';
	import {
		conversionInto,
		FUNNEL_PAGE,
		FUNNEL_STAGES,
		lossRows,
		type FunnelStage,
		type Overview
	} from './model';
	import type { InsightsQuery } from './query';

	interface Props {
		overview: Overview;
		query: InsightsQuery;
		rangeText: string;
	}
	let { overview, query, rangeText }: Props = $props();

	const locale = $derived(currentLocale());
	const total = $derived(overview.funnel_total);
	const bucket = $derived(overview.range.bucket);
	const tz = $derived(overview.range.tz);

	const stageName = (s: FunnelStage) =>
		({
			harvested: m.insights_overview_funnel_harvested(),
			peer_found: m.insights_overview_funnel_peer_found(),
			fetched: m.insights_overview_funnel_fetched(),
			stored: m.insights_overview_funnel_stored(),
			searchable: m.insights_overview_funnel_searchable()
		})[s];
	const reasonName = (r: string) => {
		const known: Record<string, () => string> = {
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
		return known[r]?.() ?? r;
	};

	const scale = $derived(Math.max(0, ...FUNNEL_STAGES.map((s) => total.stages[s])));
	const stages = $derived<BarItem[]>(
		FUNNEL_STAGES.map((s) => {
			const conv = conversionInto(total, s);
			return {
				key: s,
				label: stageName(s),
				href: stageHref(FUNNEL_PAGE[s] as Stage, query),
				value: total.stages[s],
				text: formatCount(total.stages[s], locale),
				sub:
					s === 'harvested'
						? undefined
						: m.insights_overview_funnel_conversion({ percent: formatPercent(conv, locale, 1) })
			};
		})
	);
	const losses = $derived(lossRows(total));

	// Cohorts: the latest ones first-hand, the rest on request.
	const LATEST = 24;
	let all = $state(false);
	const newest = $derived([...overview.funnel].reverse());
	const shownCohorts = $derived(all ? newest : newest.slice(0, LATEST));
	const cohortScale = $derived(Math.max(0, ...shownCohorts.map((c) => c.stages.harvested)));
	const cohorts = $derived<BarItem[]>(
		shownCohorts.map((c) => ({
			key: String(c.cohort),
			label: formatDateFull(c.cohort, locale, { timeZone: tz, bucket }),
			value: c.stages.harvested,
			text: formatCount(c.stages.harvested, locale),
			sub: m.insights_overview_cohort_searchable({
				count: formatCount(c.stages.searchable, locale)
			}),
			hatched: !c.final,
			tag: c.final ? undefined : m.incomplete()
		}))
	);
</script>

<p class="ins-summary" data-testid="insights-funnel-complete">
	{m.insights_overview_cohorts_complete({
		final: formatCount(total.final_cohorts, locale),
		total: formatCount(total.cohorts, locale)
	})}
	{m.insights_overview_cohorts_note()}
</p>

<div class="ins-pair">
	<ChartPanel
		id="ins-funnel"
		title={m.insights_overview_funnel_title()}
		summary={m.insights_overview_funnel_summary({ range: rangeText })}
		caption={m.insights_overview_funnel_title()}
		columns={[
			{ key: 'stage', label: m.insights_overview_col_stage() },
			{ key: 'count', label: m.insights_overview_col_count(), numeric: true },
			{ key: 'conversion', label: m.insights_overview_col_conversion(), numeric: true }
		]}
		rows={FUNNEL_STAGES.map((s) => ({
			stage: stageName(s),
			count: formatCount(total.stages[s], locale),
			conversion: formatPercent(conversionInto(total, s), locale, 1)
		}))}
		csv="funnel"
		{query}
		testid="insights-funnel-panel"
	>
		<LinkedBars
			items={stages}
			max={scale}
			ordered
			label={m.insights_overview_funnel_title()}
			testid="insights-funnel"
		/>
	</ChartPanel>

	<ChartPanel
		id="ins-losses"
		title={m.insights_overview_losses_title()}
		summary={m.insights_overview_losses_summary({ range: rangeText })}
		caption={m.insights_overview_losses_title()}
		columns={[
			{ key: 'reason', label: m.insights_overview_col_reason() },
			{ key: 'count', label: m.insights_overview_col_count(), numeric: true },
			{ key: 'share', label: m.insights_overview_col_share(), numeric: true }
		]}
		rows={losses.map((l) => ({
			reason: reasonName(l.reason),
			count: formatCount(l.count, locale),
			share: formatPercent(l.share, locale, 0)
		}))}
		csv="losses"
		{query}
		testid="insights-losses-panel"
	>
		{#if losses.length === 0}
			<p data-testid="insights-losses-none">{m.insights_overview_losses_none()}</p>
		{:else}
			<div
				class="ins-stack"
				role="img"
				aria-label={m.insights_overview_losses_title()}
				data-testid="insights-losses-bar"
			>
				{#each losses as l (l.reason)}
					<span style:flex="{l.share ?? 0} 1 0" title={reasonName(l.reason)}></span>
				{/each}
			</div>
			<ul class="ins-list" data-testid="insights-losses">
				{#each losses as l (l.reason)}
					<li class="ins-item-row" data-testid="insights-loss-{l.reason}">
						<a
							class="ins-link"
							href={resolve(stageHref('fetch', query, { reason: l.reason }) as Route)}
							>{reasonName(l.reason)}</a
						>
						<span class="ins-num">
							<span data-testid="insights-loss-{l.reason}-count"
								>{formatCount(l.count, locale)}</span
							>
							<span class="ins-muted" data-testid="insights-loss-{l.reason}-share"
								>{formatPercent(l.share, locale, 0)}</span
							>
						</span>
					</li>
				{/each}
			</ul>
		{/if}
	</ChartPanel>
</div>

<ChartPanel
	id="ins-cohorts"
	title={m.insights_overview_cohorts_title()}
	summary={m.insights_overview_cohorts_summary({ range: rangeText })}
	caption={m.insights_overview_cohorts_title()}
	columns={[
		{ key: 'cohort', label: m.insights_overview_col_cohort() },
		{ key: 'harvested', label: m.insights_overview_funnel_harvested(), numeric: true },
		{ key: 'searchable', label: m.insights_overview_funnel_searchable(), numeric: true },
		{ key: 'final', label: m.insights_overview_col_status() }
	]}
	rows={newest.map((c) => ({
		cohort: formatDateFull(c.cohort, locale, { timeZone: tz, bucket }),
		harvested: formatCount(c.stages.harvested, locale),
		searchable: formatCount(c.stages.searchable, locale),
		final: c.final ? m.insights_overview_cohort_complete() : m.incomplete()
	}))}
	{query}
	testid="insights-cohorts-panel"
>
	{#if cohorts.length === 0}
		<p>{m.insights_overview_cohorts_none()}</p>
	{:else}
		<div class="ins-cohorts">
			<LinkedBars
				items={cohorts}
				max={cohortScale}
				label={m.insights_overview_cohorts_title()}
				testid="insights-cohort"
			/>
		</div>
		{#if !all && newest.length > LATEST}
			<Button variant="quiet" dense onclick={() => (all = true)} data-testid="insights-cohorts-all">
				{m.insights_overview_cohorts_all({ count: formatCount(total.cohorts, locale) })}
			</Button>
		{/if}
	{/if}
</ChartPanel>
