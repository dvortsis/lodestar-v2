<script lang="ts">
	import CategoryBar from '$lib/charts/CategoryBar.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { Cell } from './model';

	interface Props {
		cell: Cell;
		/** The server's `scale_bytes`: every bar divides by it, so the bars compare. */
		max: number;
		mono?: boolean;
	}
	let { cell, max, mono = false }: Props = $props();
</script>

{#if cell.composition !== undefined}
	{#if cell.composition}
		<CategoryBar
			segments={cell.composition.segments}
			summary={cell.composition.summary}
			dominant={cell.composition.dominant}
			{max}
			testid={cell.testid}
		/>
	{:else}
		<span class="cmp-muted" data-testid={cell.testid}>{m.compare_not_classified_yet()}</span>
	{/if}
{:else if cell.text === null}
	<span data-testid={cell.testid}
		><span aria-hidden="true">—</span><span class="sr-only">{cell.name}</span></span
	>
{:else}
	<span class={mono ? 'font-mono' : ''} data-testid={cell.testid}>{cell.text}</span>
{/if}

<style>
	.cmp-muted {
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
		color: var(--color-muted-text);
	}
</style>
