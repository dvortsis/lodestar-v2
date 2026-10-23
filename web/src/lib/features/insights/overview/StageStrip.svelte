<script lang="ts">
	// Orientation (§10.9.1): the five stages with one headline number each, as an ordered list of
	// links that move you to the level explaining the stage. The list is its own text equivalent:
	// the exact count is visually hidden beside the compact one. Discover is always approximate.
	import { formatCompact, formatCount, type MetricValue } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { STAGES, type Stage } from './links';
	import { STRIP_TARGET, type Overview } from './model';

	interface Props {
		stages: Overview['stages'];
		onjump: (target: string) => void;
	}
	let { stages, onjump }: Props = $props();

	const locale = $derived(currentLocale());
	const names: Record<Stage, () => string> = {
		discover: () => m.insights_stage_discover(),
		fetch: () => m.insights_stage_fetch(),
		understand: () => m.insights_stage_understand(),
		index: () => m.insights_stage_index(),
		serve: () => m.insights_stage_serve()
	};
	const hints: Record<Stage, () => string> = {
		discover: () => m.insights_overview_stage_discover_hint(),
		fetch: () => m.insights_overview_stage_fetch_hint(),
		understand: () => m.insights_overview_stage_understand_hint(),
		index: () => m.insights_overview_stage_index_hint(),
		serve: () => m.insights_overview_stage_serve_hint()
	};
	// Why a stage has no number, for the accessible name next to the dash.
	const missing: Record<Stage, () => string> = {
		discover: () => m.insights_overview_not_measured(),
		fetch: () => m.insights_overview_not_measured(),
		understand: () => m.insights_overview_not_measured(),
		index: () => m.insights_overview_search_off(),
		serve: () => m.insights_overview_not_measured()
	};
	const value = (stage: Stage): MetricValue =>
		stage === 'discover' ? { value: stages.discover, approx: true } : stages[stage];
</script>

<nav aria-label={m.insights_overview_strip_label()} data-testid="insights-strip">
	<ol class="ins-strip">
		{#each STAGES as stage (stage)}
			{@const v = value(stage)}
			<li>
				<a
					class="ins-stage"
					href="#insights-{STRIP_TARGET[stage]}"
					data-testid="insights-stage-{stage}"
					onclick={(e) => {
						e.preventDefault();
						onjump(STRIP_TARGET[stage]);
					}}
				>
					<span class="ins-stage-name">{names[stage]()}</span>
					{#if stages[stage] === null}
						<span
							class="ins-stage-value"
							aria-hidden="true"
							data-testid="insights-stage-{stage}-value">—</span
						>
						<span class="sr-only">{missing[stage]()}</span>
					{:else}
						<span
							class="ins-stage-value"
							aria-hidden="true"
							data-testid="insights-stage-{stage}-value">{formatCompact(v, locale)}</span
						>
						<span class="sr-only" data-testid="insights-stage-{stage}-exact"
							>{formatCount(v, locale)}</span
						>
					{/if}
					<span class="ins-muted">{hints[stage]()}</span>
				</a>
			</li>
		{/each}
	</ol>
</nav>
