<script lang="ts">
	// Level 7, Detail (§10.9.2): one link per stage page, keeping the range and Compare.
	import { m } from '$lib/paraglide/messages.js';
	import { resolve } from '$app/paths';
	import type { Route } from './href';
	import { stageHref, STAGES, type Stage } from './links';
	import type { InsightsQuery } from './query';

	let { query }: { query: InsightsQuery } = $props();

	const names: Record<Stage, () => string> = {
		discover: () => m.insights_stage_discover(),
		fetch: () => m.insights_stage_fetch(),
		understand: () => m.insights_stage_understand(),
		index: () => m.insights_stage_index(),
		serve: () => m.insights_stage_serve()
	};
	const hints: Record<Stage, () => string> = {
		discover: () => m.insights_overview_detail_discover(),
		fetch: () => m.insights_overview_detail_fetch(),
		understand: () => m.insights_overview_detail_understand(),
		index: () => m.insights_overview_detail_index(),
		serve: () => m.insights_overview_detail_serve()
	};
</script>

<ul class="ins-detail" data-testid="insights-detail">
	{#each STAGES as stage (stage)}
		<li class="ins-card">
			<a
				class="ins-link target ins-card-title"
				href={resolve(stageHref(stage, query, { compare: true }) as Route)}
				data-testid="insights-detail-{stage}">{names[stage]()}</a
			>
			<span class="ins-muted">{hints[stage]()}</span>
		</li>
	{/each}
</ul>
