<script lang="ts">
	// Placeholders at each panel's final height, so the page does not shift when data arrives.
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import { m } from '$lib/paraglide/messages.js';
</script>

<div class="ins-page-body" aria-busy="true" data-testid="insights-loading">
	<span class="sr-only" role="status">{m.insights_overview_loading()}</span>
	<Skeleton class="ins-sk-strip" />
	<div class="ins-kpis">
		{#each [1, 2, 3, 4, 5] as n (n)}<Skeleton class="ins-sk-kpi" />{/each}
	</div>
	<Skeleton class="ins-sk-chart" />
	<Skeleton class="ins-sk-panel" />
</div>
