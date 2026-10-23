<script lang="ts">
	import type { Component } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';

	// The overview loads as its own chunk, so this route's entry stays small: the app chunk lists a
	// route's static dependencies for preload, and /search shares that chunk.
	let Overview = $state.raw<Component>();
	let failed = $state(false);
	let attempt = $state(0);
	$effect(() => {
		void attempt;
		failed = false;
		import('$lib/features/insights/overview/Overview.svelte').then(
			(mod) => (Overview = mod.default),
			() => (failed = true)
		);
	});
</script>

{#if Overview}
	<Overview />
{:else if failed}
	<div class="flex flex-col gap-4 p-6" role="alert" data-testid="insights-load-error">
		<p>{m.insights_overview_chunk_failed()}</p>
		<button type="button" class="btn btn-primary" onclick={() => (attempt += 1)}
			>{m.insights_overview_retry()}</button
		>
	</div>
{/if}
