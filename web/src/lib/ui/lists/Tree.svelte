<script lang="ts">
	// WAI-ARIA tree over a flat, pre-ordered node array, virtualised through VirtualList. Only the
	// visible rows exist as an array (the flattened row model), so 100k nodes stay cheap.
	// Checkboxes are tri-state on directories ("Send selected files"); sizes arrive formatted.
	import IconCaretRight from '~icons/ph/caret-right';
	import IconFolder from '~icons/ph/folder';
	import IconFile from '~icons/ph/file';
	import VirtualList from './VirtualList.svelte';
	import {
		buildIndex,
		checkedCounts,
		checkState,
		toggleChecked,
		visibleIndices,
		type FlatNode
	} from './tree-model';
	import './lists.css';

	interface Props {
		nodes: FlatNode[];
		label: string;
		/** Ids of expanded directories (controlled). */
		expanded?: ReadonlySet<string>;
		onExpandedChange?: (expanded: Set<string>) => void;
		/** Ids of checked files (controlled). Omit `onCheckedChange` for a tree without checkboxes. */
		checked?: ReadonlySet<string>;
		onCheckedChange?: (checked: Set<string>) => void;
		/** Accessible name for a node's checkbox, e.g. "Select {name}". */
		checkLabel?: (node: FlatNode) => string;
		onActivate?: (node: FlatNode) => void;
		/** Row height in px. */
		rowHeight?: number;
		class?: string;
	}
	let {
		nodes,
		label,
		expanded = new Set<string>(),
		onExpandedChange,
		checked = new Set<string>(),
		onCheckedChange,
		checkLabel,
		onActivate,
		rowHeight = 36,
		class: className = ''
	}: Props = $props();

	const checkable = $derived(!!onCheckedChange);
	const index = $derived(buildIndex(nodes));
	const visible = $derived(visibleIndices(nodes, index, expanded));
	const counts = $derived(checkable ? checkedCounts(nodes, index, checked) : null);

	let list = $state<VirtualList<number>>();
	let activeIndex = $state(0);

	const node = (i: number) => nodes[i] as FlatNode;

	/** Position in `visible` of node `i` (binary search; `visible` is ascending). */
	function position(i: number): number {
		let lo = 0;
		let hi = visible.length - 1;
		while (lo <= hi) {
			const mid = (lo + hi) >> 1;
			const v = visible[mid] as number;
			if (v === i) return mid;
			if (v < i) lo = mid + 1;
			else hi = mid - 1;
		}
		return -1;
	}

	function setExpanded(ids: Iterable<string>, on: boolean) {
		// eslint-disable-next-line svelte/prefer-svelte-reactivity -- a plain copy of controlled state, never mutated after the callback
		const next = new Set(expanded);
		for (const id of ids) {
			if (on) next.add(id);
			else next.delete(id);
		}
		onExpandedChange?.(next);
	}

	function onKey(event: KeyboardEvent, ctx: { index: number; item: number; onRow: boolean }) {
		if (!ctx.onRow) return;
		const i = ctx.item;
		const n = node(i);
		switch (event.key) {
			case 'ArrowRight':
				event.preventDefault();
				if (n.dir && !expanded.has(n.id)) setExpanded([n.id], true);
				else if (n.dir && (index.end[i] as number) > i + 1) list?.moveTo(ctx.index + 1);
				break;
			case 'ArrowLeft': {
				event.preventDefault();
				const parent = index.parent[i] as number;
				if (n.dir && expanded.has(n.id)) setExpanded([n.id], false);
				else if (parent >= 0) list?.moveTo(position(parent));
				break;
			}
			case '*': {
				event.preventDefault();
				// Expand every directory that shares this node's parent.
				const parent = index.parent[i] as number;
				const siblings: string[] = [];
				for (let j = parent + 1; j < (parent < 0 ? nodes.length : (index.end[parent] as number));) {
					if (index.parent[j] === parent && node(j).dir) siblings.push(node(j).id);
					j = index.parent[j] === parent ? (index.end[j] as number) : j + 1;
				}
				setExpanded(siblings, true);
				break;
			}
			case ' ':
				if (checkable) {
					event.preventDefault();
					onCheckedChange?.(toggleChecked(nodes, index, checked, i));
				}
				break;
		}
	}

	function attrs(i: number) {
		const n = node(i);
		const a: Record<string, string | number | boolean | undefined> = {
			'aria-level': n.depth + 1,
			'aria-posinset': index.posinset[i],
			'aria-setsize': index.setsize[i],
			'aria-expanded': n.dir ? expanded.has(n.id) : undefined
		};
		if (counts) {
			const s = checkState(index, counts, i);
			a['aria-checked'] = s === 'mixed' ? 'mixed' : s === 'on';
		}
		return a;
	}

	function mixed(el: HTMLInputElement, value: boolean) {
		el.indeterminate = value;
		return {
			update(v: boolean) {
				el.indeterminate = v;
			}
		};
	}
</script>

<VirtualList
	bind:this={list}
	bind:activeIndex
	items={visible}
	getKey={(i) => node(i).id}
	{label}
	role="tree"
	estimateSize={rowHeight}
	multiselectable={checkable}
	getText={(i) => node(i).name}
	itemAttrs={(i) => attrs(i)}
	{onKey}
	onActivate={(_, i) => {
		const n = node(i);
		if (n.dir) setExpanded([n.id], !expanded.has(n.id));
		onActivate?.(n);
	}}
	class={className}
>
	{#snippet row(i: number)}
		{@const n = node(i)}
		<div
			class="flex items-center gap-2 pe-3 text-body hover:bg-hover"
			style:min-height="{rowHeight}px"
			style:padding-inline-start="calc(var(--spacing) * {n.depth * 4 + 2})"
		>
			{#if n.dir}
				<IconCaretRight
					class="size-(--icon-size-row) shrink-0 {expanded.has(n.id) ? 'rotate-90' : ''}"
					aria-hidden="true"
				/>
			{:else}
				<span class="size-(--icon-size-row) shrink-0" aria-hidden="true"></span>
			{/if}
			{#if counts}
				{@const state = checkState(index, counts, i)}
				<input
					type="checkbox"
					tabindex="-1"
					class="checkbox checkbox-sm"
					aria-label={checkLabel?.(n)}
					checked={state === 'on'}
					use:mixed={state === 'mixed'}
					onclick={(e) => {
						e.stopPropagation();
						onCheckedChange?.(toggleChecked(nodes, index, checked, i));
					}}
				/>
			{/if}
			{#if n.dir}
				<IconFolder class="size-(--icon-size-row) shrink-0 text-muted-text" aria-hidden="true" />
			{:else}
				<IconFile class="size-(--icon-size-row) shrink-0 text-muted-text" aria-hidden="true" />
			{/if}
			<span class="min-w-0 flex-1 truncate">{n.name}</span>
			{#if n.size}
				<span class="shrink-0 text-small text-muted-text tabular-nums">{n.size}</span>
			{/if}
		</div>
	{/snippet}
</VirtualList>
