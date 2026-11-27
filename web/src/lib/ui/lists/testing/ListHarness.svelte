<script lang="ts">
	import VirtualList from '../VirtualList.svelte';
	interface Item {
		id: string;
		name: string;
	}
	let {
		items,
		hasMore = false,
		loadingMore = false,
		onLoadMore,
		onActiveChange,
		onActivate,
		onKey,
		resetKey,
		withControl = false
	}: {
		items: Item[];
		hasMore?: boolean;
		loadingMore?: boolean;
		onLoadMore?: () => void;
		onActiveChange?: (i: number, item: Item) => void;
		onActivate?: (i: number, item: Item) => void;
		onKey?: (e: KeyboardEvent, ctx: { index: number; item: Item; onRow: boolean }) => void;
		resetKey?: unknown;
		/** A button inside each row, like a card tile's. */
		withControl?: boolean;
	} = $props();
	let activeIndex = $state(0);
</script>

<button type="button">before</button>
<VirtualList
	{items}
	getKey={(it) => it.id}
	label="Results"
	estimateSize={36}
	bind:activeIndex
	getText={(it) => it.name}
	{hasMore}
	{loadingMore}
	{onLoadMore}
	loadMoreLabel="Loading more"
	{onActiveChange}
	{onActivate}
	{onKey}
	{resetKey}
>
	{#snippet row(item, index, state)}
		<div data-active={state.active} data-row={index}>
			{item.name}{#if withControl}<button type="button" tabindex="-1">open</button>{/if}
		</div>
	{/snippet}
</VirtualList>
<span data-testid="active">{activeIndex}</span>
