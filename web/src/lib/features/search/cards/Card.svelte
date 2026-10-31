<script lang="ts">
	// One result as a tile (§10.4 Cards density): the same hit and the same actions as the row. Opening
	// is the tile's (click, Enter); the actions are real buttons inside it. Styles: cards.css.
	import type { Snippet } from 'svelte';
	import ArrowDown from '~icons/ph/arrow-down-bold';
	import ArrowUp from '~icons/ph/arrow-up-bold';
	import { formatBytes, formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import { showBidi } from '$lib/util/bidi';
	import { ADULT_LABELS as ADULT, TYPE_LABELS as TYPE } from '$lib/util/content-labels';
	import type { SearchHit } from '../results/ResultRow.svelte';

	interface Props {
		hit: SearchHit;
		testid: string;
		onopen: (hit: SearchHit) => void;
		actions?: Snippet<[SearchHit]>;
	}
	let { hit, testid: id, onopen, actions }: Props = $props();

	const locale = $derived(currentLocale());
	const title = $derived(showBidi(hit.title ?? hit.name));
	const type = $derived(
		hit.content_type === 'adult' && hit.adult_kind
			? ADULT[hit.adult_kind]?.()
			: hit.content_type
				? TYPE[hit.content_type]?.()
				: undefined
	);

	function key(event: KeyboardEvent) {
		if (event.target !== event.currentTarget || event.key !== 'Enter') return;
		event.preventDefault();
		event.stopPropagation();
		onopen(hit);
	}
	function click(event: MouseEvent) {
		if ((event.target as HTMLElement).closest('button,a')) return;
		event.stopPropagation();
		onopen(hit);
	}
</script>

<!-- The tile is the open target, as the row is for the list; its buttons stay buttons. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
	class="cd-tile"
	role="group"
	aria-label={title}
	tabindex="0"
	data-testid={id}
	data-hash={hit.hash}
	onclick={click}
	onkeydown={key}
>
	<div class="cd-title" data-testid="{id}-title">{title}</div>
	{#if type}
		<div class="cd-badges"><span class="cd-badge" data-testid="{id}-badge-type">{type}</span></div>
	{/if}
	<div class="cd-facts">
		<span class="cd-fact" data-testid="{id}-size">{formatBytes(hit.size, locale)}</span>
		<span class="cd-fact" data-testid="{id}-files"
			>{(hit.files === 1 ? m.search_results_files_one : m.search_results_files_other)({
				count: formatCount(hit.files, locale)
			})}</span
		>
		<span class="cd-fact" data-testid="{id}-swarm">
			<span class="sr-only">{m.seeders()}: </span><ArrowUp aria-hidden="true" />
			{#if hit.seeders === null}
				<span role="img" aria-label={m.never_scraped()} data-testid="{id}-never-scraped">—</span>
			{:else}
				<span data-testid="{id}-seeders"
					>{formatCount({ value: hit.seeders, approx: hit.seeders_approx }, locale)}</span
				>
			{/if}
			<span class="sr-only">{m.leechers()}: </span><ArrowDown aria-hidden="true" />
			{#if hit.leechers === null}
				<span role="img" aria-label={m.never_scraped()}>—</span>
			{:else}
				<span data-testid="{id}-leechers"
					>{formatCount({ value: hit.leechers, approx: hit.seeders_approx }, locale)}</span
				>
			{/if}
		</span>
		<span class="cd-fact">
			<span class="sr-only">{m.discovered()}: </span>
			<span data-testid="{id}-discovered"><RelativeTime seconds={hit.discovered_at} /></span>
		</span>
	</div>
	{#if actions}<div class="cd-actions rr-actions">{@render actions(hit)}</div>{/if}
</div>
