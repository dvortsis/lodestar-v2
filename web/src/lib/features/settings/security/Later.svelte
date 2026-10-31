<script lang="ts">
	// A section whose body loads on request: its heading and a Show button are in the first view,
	// the body (the key manager, the proxy settings) loads when the button is pressed, so it never
	// counts against the first view. Focus moves to the heading once the body is in.
	import { tick, type Component } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';

	let {
		id,
		heading,
		help,
		show,
		load
	}: {
		id: string;
		heading: string;
		help: string;
		show: string;
		load: () => Promise<{ default: Component }>;
	} = $props();
	let Part = $state<Component | false>();
	let busy = $state(false);
	let title = $state<HTMLElement>();

	async function start() {
		if (busy) return;
		busy = true;
		try {
			Part = (await load()).default;
			await tick();
			title?.focus();
		} catch {
			Part = false;
		} finally {
			busy = false;
		}
	}
</script>

<section class="flex flex-col gap-4" aria-labelledby={id}>
	<h2 {id} bind:this={title} tabindex="-1" class="text-label font-semibold">{heading}</h2>
	{#if Part}
		<Part />
	{:else}
		<p class="text-muted-text">{help}</p>
		{#if Part === false}
			<p class="text-small">{m.settings_security_part_failed()}</p>
		{/if}
		<div>
			<Button loading={busy} aria-expanded="false" onclick={start} data-testid="{id}-show"
				>{Part === false ? m.settings_security_retry() : show}</Button
			>
		</div>
	{/if}
</section>
