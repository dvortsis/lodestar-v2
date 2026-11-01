<script lang="ts">
	// A section whose body loads on its own right after the first paint (a dynamic import after
	// mount, as the Settings connection screens' parts load), with a skeleton meanwhile. The body
	// stays out of the first-view gate without costing the user a click. A body that can't load
	// says so and offers Try again.
	import { onMount, type Component } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';

	let {
		id,
		heading,
		load
	}: { id: string; heading: string; load: () => Promise<{ default: Component }> } = $props();
	let Part = $state<Component | false>();

	function start() {
		Part = undefined;
		load().then(
			(p) => (Part = p.default),
			() => (Part = false)
		);
	}
	onMount(start);
</script>

<section class="flex flex-col gap-4" aria-labelledby={id} aria-busy={Part === undefined}>
	<h2 {id} class="text-small font-semibold">{heading}</h2>
	{#if Part}
		<Part />
	{:else if Part === false}
		<p class="flex flex-wrap items-center gap-3">
			{m.settings_security_part_failed()}
			<Button onclick={start} data-testid="{id}-retry">{m.settings_security_retry()}</Button>
		</p>
	{:else}
		<p role="status" class="sr-only">{m.settings_security_loading()}</p>
		<Skeleton lines={3} />
	{/if}
</section>
