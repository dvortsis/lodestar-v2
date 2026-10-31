<script lang="ts">
	// A part of the screen that loads after the first view, with its loading and failed states.
	import type { Component } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';

	let { load }: { load: () => Promise<{ default: Component }> } = $props();
	let Part = $state<Component | false>();
	const start = () => {
		Part = undefined;
		load().then(
			(p) => (Part = p.default),
			() => (Part = false)
		);
	};
	start();
</script>

{#if Part}
	<Part />
{:else if Part === false}
	<p class="flex flex-wrap items-center gap-3">
		{m.settings_security_part_failed()}
		<Button onclick={start}>{m.settings_security_retry()}</Button>
	</p>
{:else}
	<p aria-busy="true" role="status">{m.settings_security_loading()}</p>
{/if}
