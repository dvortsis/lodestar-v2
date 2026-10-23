<script lang="ts">
	import type { Component } from 'svelte';

	// The overview loads as its own chunk, so this route's entry stays small: the app chunk lists a
	// route's static dependencies for preload, and /search shares that chunk.
	let Overview = $state.raw<Component>();
	$effect(() => {
		void import('$lib/features/insights/overview/Overview.svelte').then(
			(m) => (Overview = m.default)
		);
	});
</script>

{#if Overview}<Overview />{/if}
