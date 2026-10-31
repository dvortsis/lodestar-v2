<script lang="ts">
	// Cards density (§10.4): the results as rows of cards in the same virtual list. The panel loads
	// this when `density=cards` and keeps `active` as an index into the hits, as for the rows; here a
	// list row is `cols` cards, so the two indexes are mapped. Styles: cards.css.
	import { untrack } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import VirtualList from '$lib/ui/lists/VirtualList.svelte';
	import { tokenPx } from '$lib/ui/overlays/motion';
	import type { SearchHit } from '../results/ResultRow.svelte';
	import { results } from '../results/results.svelte';
	import Card from './Card.svelte';
	import { chunk, observeColumns } from './cards';
	import './cards.css';

	interface Props {
		open: (hit: SearchHit) => void;
		/** Index of the active hit (the panel's `j`/`k` and the drawer follow it). */
		active: number;
	}
	let { open, active = $bindable(0) }: Props = $props();

	// Search's row actions, the chunk the rows use.
	let Actions = $state<typeof import('../results/RowActions.svelte').default>();
	void import('../results/RowActions.svelte').then((mod) => (Actions = mod.default));

	let host = $state<HTMLElement>();
	let cols = $state(1);
	let at = $state(0);
	let vl = $state<{ move: (d: number) => void; moveTo: (i: number) => void }>();
	const rows = $derived(chunk(results.hits, cols));
	// Cards are three comfortable rows tall (three lines of facts, the title and the actions).
	const height = tokenPx('--row-height-comfortable', 64) * 3;

	$effect(() => {
		const el = host?.closest<HTMLElement>('.rp-list');
		return el ? observeColumns(el, (n) => (cols = n)) : undefined;
	});
	$effect(() => {
		const r = Math.floor(active / cols);
		untrack(() => (at = r));
	});
	$effect(() => {
		const r = at;
		untrack(() => {
			if (Math.floor(active / cols) !== r) active = r * cols;
		});
	});

	export const move = (delta: number) => vl?.move(delta);
	export const moveTo = (i: number) => vl?.moveTo(Math.floor(i / cols));
</script>

<div class="cd-host" bind:this={host}>
	<VirtualList
		bind:this={vl}
		items={rows}
		role="grid"
		getKey={(r) => r.key}
		label={m.search_cards_label()}
		estimateSize={height}
		resetKey={results.shown ?? ''}
		bind:activeIndex={at}
		onActivate={(_i, r) => open(r.items[0]!)}
		hasMore={results.hasMore}
		loadingMore={results.loadingMore}
		onLoadMore={() => void results.loadMore()}
		loadMoreLabel={m.search_results_loading_more()}
	>
		{#snippet row(r)}
			<div class="cd-row" style:--cd-cols={cols} role="presentation">
				{#each r.items as hit (hit.hash)}
					<div role="gridcell" class="cd-cell">
						<Card {hit} testid="result-{hit.hash}" onopen={open}>
							{#snippet actions(h)}
								{#if Actions}<Actions hit={h} density="comfortable" testid="result-{h.hash}" />{/if}
							{/snippet}
						</Card>
					</div>
				{/each}
			</div>
		{/snippet}
	</VirtualList>
</div>
