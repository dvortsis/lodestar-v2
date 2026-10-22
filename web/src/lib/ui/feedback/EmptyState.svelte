<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		title: string;
		text?: string;
		/** The one next step (a Button or link). */
		action?: Snippet;
		/** Optional illustration; features pass it in, the primitive imports no brand assets. */
		illustration?: Snippet;
		headingLevel?: 2 | 3 | 4;
		class?: string;
	}

	let {
		title,
		text,
		action,
		illustration,
		headingLevel = 2,
		class: className = ''
	}: Props = $props();
</script>

<div class="flex flex-col items-center gap-4 p-6 text-center {className}">
	{#if illustration}
		<div class="h-(--art-height-small) large:h-(--art-height-large)" aria-hidden="true">
			{@render illustration()}
		</div>
	{/if}
	<div class="flex max-w-(--prose-measure) flex-col gap-1">
		<svelte:element this={`h${headingLevel}`} class="text-title font-semibold"
			>{title}</svelte:element
		>
		{#if text}<p class="text-body text-muted-text">{text}</p>{/if}
	</div>
	{#if action}{@render action()}{/if}
</div>
