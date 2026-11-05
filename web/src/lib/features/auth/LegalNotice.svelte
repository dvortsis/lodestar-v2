<script lang="ts">
	// The Legal Notice, rendered from its Markdown node by node (never as HTML), in a region that
	// scrolls on its own and takes keyboard focus so it can be scrolled without a pointer. The
	// notice's own headings sit below the page's: `#` → h2, `##` → h3.
	import { parseBlocks, type Inline } from './markdown';

	interface Props {
		text: string;
		/** The region's accessible name. */
		label: string;
		/** Scroll inside a box (setup, sign-in) or flow with the page (About). */
		scroll?: boolean;
		/** The level the notice's own `#` heading takes, below the page's headings; `##` is one lower. */
		level?: 2 | 3;
	}

	let { text, label, scroll = true, level = 2 }: Props = $props();
	const blocks = $derived(parseBlocks(text));
	const LINK = 'ls-focus break-all text-primary underline';
</script>

{#snippet link(href: string, text: string)}
	<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- external https only (markdown.ts) -->
	<a class={LINK} {href} target="_blank" rel="noopener noreferrer">{text}</a>
{/snippet}

{#snippet inlines(nodes: Inline[])}
	{#each nodes as n, i (i)}
		{#if n.t === 'text'}{n.v}{:else if n.t === 'strong'}<strong class="font-semibold"
				>{@render inlines(n.c)}</strong
			>{:else if n.t === 'em'}<em>{@render inlines(n.c)}</em>{:else if n.t === 'code'}<code
				class="font-mono">{n.v}</code
			>{:else}{@render link(n.href, n.v)}{/if}
	{/each}
{/snippet}

<!-- In a box it has a fixed height, the same as the loading line, so the notice arriving moves
     nothing. The region takes focus so a keyboard user can scroll it (axe:
     scrollable-region-focusable). -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<section
	aria-label={label}
	tabindex={scroll ? 0 : undefined}
	data-testid="legal-notice"
	class={[
		'ls-focus flex flex-col gap-3 text-small text-base-content',
		scroll && 'h-(--notice-max-height) overflow-y-auto rounded-box border border-base-300 p-4'
	]}
>
	{#each blocks as b, i (i)}
		{#if b.t === 'heading' && b.level === 1 && level === 2}
			<h2 class="text-title font-semibold">{@render inlines(b.c)}</h2>
		{:else if b.t === 'heading' && (b.level === 1 || level === 2)}
			<!-- `#` under a screen heading, or `##` on its own page -->
			<h3 class={b.level === 1 ? 'text-title font-semibold' : 'mt-2 font-semibold'}>
				{@render inlines(b.c)}
			</h3>
		{:else if b.t === 'heading'}
			<h4 class="mt-2 font-semibold">{@render inlines(b.c)}</h4>
		{:else if b.t === 'ul'}
			<ul class="flex list-disc flex-col gap-1 pl-5">
				{#each b.items as item, j (j)}
					<li>{@render inlines(item)}</li>
				{/each}
			</ul>
		{:else}
			<p>{@render inlines(b.c)}</p>
		{/if}
	{/each}
</section>
