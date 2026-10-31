<script lang="ts">
	// Cards density (§10.4): the results as rows of cards in the same virtual list. The panel loads
	// this when `density=cards` and keeps `active` as an index into the hits (j/k, `x`, the drawer
	// follow); here a list row is `cols` tiles, so the active tile is the roving one: arrows, j/k and
	// a click or focus on a tile all move `active`. Styles: cards.css.
	import { tick, untrack } from 'svelte';
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
	let vl = $state<{ moveTo: (i: number) => void }>();
	const rows = $derived(chunk(results.hits, cols));
	// Four comfortable rows tall: the title (two lines), the badges, the facts (they wrap on a narrow
	// tile) and the actions, which are 44 px under a coarse pointer.
	const height = tokenPx('--row-height-comfortable', 64) * 4;

	// One tab stop: the list gives its active row tabindex 0, but the roving item here is the tile.
	$effect(() => {
		if (!host) return;
		const el = host;
		const demote = () =>
			el
				.querySelectorAll('[role="row"][tabindex="0"]')
				.forEach((r) => r.setAttribute('tabindex', '-1'));
		demote();
		const mo = new MutationObserver(demote);
		mo.observe(el, {
			subtree: true,
			childList: true,
			attributes: true,
			attributeFilter: ['tabindex']
		});
		return () => mo.disconnect();
	});
	$effect(() => {
		const el = host?.closest<HTMLElement>('.rp-list');
		return el ? observeColumns(el, (n) => (cols = n)) : undefined;
	});
	// The list row follows the active hit (and the other way, for a row focused by the list itself).
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

	async function go(index: number, focus: boolean) {
		const i = Math.min(Math.max(index, 0), results.hits.length - 1);
		if (i < 0) return;
		active = i;
		vl?.moveTo(Math.floor(i / cols)); // scrolls the row in (and focuses it)
		await tick();
		if (!focus) return;
		const hash = results.hits[i]?.hash;
		host
			?.querySelector<HTMLElement>(`[data-hash="${CSS.escape(hash ?? '')}"]`)
			?.focus({ preventScroll: true });
	}
	export const move = (delta: number) => go(active + delta, true);
	export const moveTo = (i: number) => go(i, true);

	/** Arrows move between tiles (up and down by a row); the list's own keys act on rows. */
	function onKey(event: KeyboardEvent, ctx: { onRow: boolean }) {
		if (ctx.onRow) return;
		const step: Record<string, number> = {
			ArrowLeft: -1,
			ArrowRight: 1,
			ArrowUp: -cols,
			ArrowDown: cols
		};
		const by = step[event.key];
		if (by === undefined || (event.target as HTMLElement).closest('input,select,textarea')) return;
		event.preventDefault();
		void go(active + by, true);
	}
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
		{onKey}
		onActivate={(_i, r) =>
			open(r.items[Math.min(Math.max(active - r.start, 0), r.items.length - 1)]!)}
		hasMore={results.hasMore}
		loadingMore={results.loadingMore}
		onLoadMore={() => void results.loadMore()}
		loadMoreLabel={m.search_results_loading_more()}
	>
		{#snippet row(r)}
			<div class="cd-row" style:--cd-cols={cols} role="presentation">
				{#each r.items as hit, i (hit.hash)}
					<div role="gridcell" class="cd-cell">
						<Card
							{hit}
							testid="result-{hit.hash}"
							onopen={open}
							current={r.start + i === active}
							onfocusin={() => (active = r.start + i)}
						>
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
