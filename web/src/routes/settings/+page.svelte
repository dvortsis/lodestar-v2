<script lang="ts">
	import type { Component } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Banner from '$lib/ui/feedback/Banner.svelte';

	// The Overview's code loads here, on demand, so no other Settings page carries it.
	let Overview = $state<Component>();
	let failed = $state(false);
	$effect(() => {
		void import('$lib/features/settings/overview/OverviewScreen.svelte').then(
			(mod) => (Overview = mod.default),
			() => (failed = true)
		);
	});
</script>

<svelte:head><title>{m.settings_shell_title()} · Lodestar</title></svelte:head>
<h1 class="text-title font-semibold">{m.settings_shell_overview()}</h1>
<p class="mb-4">{m.settings_shell_overview_purpose()}</p>
{#if Overview}
	<Overview />
{:else if failed}
	<Banner kind="error">{m.settings_overview_failed()}</Banner>
{:else}
	<p role="status" class="text-muted-text">{m.settings_overview_loading()}</p>
{/if}
