<script lang="ts">
	// A control that loads on demand (`controls.svelte.ts`). Until it arrives, a hidden box of
	// the height it renders at holds its place, so nothing below moves.
	import { m } from '$lib/paraglide/messages.js';
	import {
		controlFailed,
		controlOf,
		lazyOf,
		loadControl,
		type LazyControlProps
	} from './controls.svelte';

	let props: LazyControlProps = $props();

	const file = $derived(lazyOf(props.builtin) ?? 'json');
	const Loaded = $derived(controlOf(file));
	const field = $derived(props.form.field(props.key));
	const options = $derived(field.spec.enum?.length ?? 0);
	$effect(() => {
		void loadControl(file);
	});
</script>

{#if Loaded}
	<Loaded {...props} />
{:else if controlFailed(file)}
	<p class="text-small text-error">{m.settings_form_chunk_failed()}</p>
{:else if props.builtin === 'json'}
	<!-- The JSON fallback's own textarea, four rows of Mono. -->
	<textarea
		class="ls-placeholder textarea w-full font-mono"
		rows="4"
		disabled
		aria-hidden="true"
		tabindex="-1"
		data-placeholder></textarea>
{:else if props.builtin === 'secret' && field.locked}
	<!-- A locked secret is only its status line. -->
	<span class="ls-placeholder ls-row text-body" aria-hidden="true" data-placeholder>&nbsp;</span>
{:else if props.builtin === 'radio'}
	<!-- One row per option, as the radio group lays them out. -->
	<div class="ls-placeholder flex flex-col gap-1" aria-hidden="true" data-placeholder>
		{#each { length: options }, i (i)}
			<span class="ls-row text-body pointer-coarse:min-h-(--target-min)">&nbsp;</span>
		{/each}
	</div>
{:else}
	<!-- One control row: an input, a select, a segmented control or a slider with its number,
	     a chip list's input or a secret's buttons all share the control height. -->
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
	.ls-row {
		display: block;
		min-height: calc(var(--spacing) * 6);
	}
</style>
