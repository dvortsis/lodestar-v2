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
		'data-testid'?: string;
		/** Test id for the body text. */
		textTestid?: string;
	}

	let {
		title,
		text,
		action,
		illustration,
		headingLevel = 2,
		class: className = '',
		'data-testid': testid,
		textTestid
	}: Props = $props();
</script>

<!-- §10.19: copy and the next step come first in the DOM. Below x-large the art sits above the text
	(order), 160 px tall (240 px from large) and hidden under 600 px of viewport height; from x-large
	it is a two-column row, text 7 / art 5 of 12, centred vertically. -->
<div
	class="es grid grid-cols-1 items-center gap-4 p-6 text-center x-large:grid-cols-12 x-large:text-start {className}"
	data-testid={testid}
>
	<div
		class="flex flex-col items-center gap-4 x-large:items-start {illustration
			? 'x-large:col-span-7'
			: 'x-large:col-span-12'}"
	>
		<div class="flex max-w-(--prose-measure) flex-col gap-1">
			{#if headingLevel === 2}
				<h2 class="text-title font-semibold">{title}</h2>
			{:else if headingLevel === 3}
				<h3 class="text-title font-semibold">{title}</h3>
			{:else}
				<h4 class="text-title font-semibold">{title}</h4>
			{/if}
			{#if text}<p class="text-body text-muted-text" data-testid={textTestid}>{text}</p>{/if}
		</div>
		{#if action}{@render action()}{/if}
	</div>
	{#if illustration}
		<div
			class="es-art order-first flex justify-center x-large:order-none x-large:col-span-5"
			aria-hidden="true"
		>
			{@render illustration()}
		</div>
	{/if}
</div>

<style>
	@media (height < 600px) {
		.es-art {
			display: none;
		}
	}
</style>
