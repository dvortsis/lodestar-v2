<script lang="ts">
	// A control that loads on demand (`controls.svelte.ts`). Until it arrives, a hidden box of
	// the height it renders at when empty holds its place, so nothing below moves.
	import { m } from '$lib/paraglide/messages.js';
	import {
		controlFailed,
		controlOf,
		loadControl,
		type LazyControlProps,
		type LazyWidget
	} from './controls.svelte';

	interface Props extends LazyControlProps {
		widget: LazyWidget;
	}

	let { widget, ...props }: Props = $props();

	const Loaded = $derived(controlOf(widget));
	$effect(() => {
		void loadControl(widget);
	});
</script>

{#if Loaded}
	<Loaded {...props} />
{:else if controlFailed(widget)}
	<p class="text-small text-error">{m.settings_form_chunk_failed()}</p>
{:else if widget === 'json'}
	<!-- The JSON fallback's own textarea, four rows of Mono. -->
	<textarea
		class="ls-placeholder textarea w-full font-mono"
		rows="4"
		disabled
		aria-hidden="true"
		tabindex="-1"
		data-placeholder></textarea>
{:else}
	<!-- One control row: the chip list's input, or the secret's buttons (the same height). -->
	<div
		class="ls-placeholder input w-full {props.dense
			? 'input-sm'
			: ''} pointer-coarse:min-h-(--target-min)"
		aria-hidden="true"
		data-placeholder
	></div>
{/if}

<style>
	.ls-placeholder {
		visibility: hidden;
	}
</style>
