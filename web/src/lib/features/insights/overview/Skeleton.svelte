<script lang="ts">
	// Placeholders at each panel's final height, so the page does not shift when data arrives.
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
</script>

<div class="ins-page-body" aria-busy="true" data-testid="insights-loading">
	<Skeleton class="ins-sk-strip" />
	<div class="ins-kpis">
		{#each [1, 2, 3, 4, 5] as n (n)}<Skeleton class="ins-sk-kpi" />{/each}
	</div>
	<Skeleton class="ins-sk-chart" />
	<Skeleton class="ins-sk-panel" />
</div>
