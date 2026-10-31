<script lang="ts">
	import type { Component } from 'svelte';
	import { page } from '$app/state';
	import UnavailablePage from '$lib/features/shell/UnavailablePage.svelte';
	import { m } from '$lib/paraglide/messages.js';

	// The stage page loads as its own chunk, so this route's entry stays small (as /insights does).
	// Discover is served by a later change; any other name is unknown.
	const STAGES = ['fetch', 'understand', 'index', 'serve'];
	const stage = $derived(page.params.stage ?? '');
	let StagePage = $state.raw<Component<{ stage: 'fetch' | 'understand' | 'index' | 'serve' }>>();
	let failed = $state(false);
	$effect(() => {
		if (!STAGES.includes(stage) || StagePage) return;
		failed = false;
		import('$lib/features/insights/stages/StagePage.svelte').then(
			(mod) => (StagePage = mod.default),
			() => (failed = true)
		);
	});
</script>

{#if !STAGES.includes(stage)}
	<UnavailablePage kind="stage" />
{:else if StagePage}
	{#key stage}
		<StagePage stage={stage as 'fetch' | 'understand' | 'index' | 'serve'} />
	{/key}
{:else if failed}
	<div class="flex flex-col gap-4 p-6" role="alert" data-testid="insights-load-error">
		<p>{m.insights_overview_chunk_failed()}</p>
		<button type="button" class="btn btn-primary" onclick={() => location.reload()}
			>{m.insights_overview_retry()}</button
		>
	</div>
{/if}
