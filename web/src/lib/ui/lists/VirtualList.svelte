<script lang="ts" generics="T">
	// Virtualised listbox / grid / tree body with a roving tabindex (§10.13). It owns scrolling,
	// the active item, arrow/Home/End/PageUp/PageDown, typeahead and the "load more" sentinel.
	// Anything else (j/k, Space, tree arrows) is the caller's: `onKey` runs first and a
	// `preventDefault()` there skips the built-ins; the shortcut registry itself belongs to W9.
	import { createVirtualizer, type VirtualItem } from '@tanstack/svelte-virtual';
	import { untrack, type Snippet } from 'svelte';
	import './lists.css';

	type Role = 'listbox' | 'grid' | 'tree' | 'treegrid';
	const ITEM_ROLE: Record<Role, string> = {
		listbox: 'option',
		grid: 'row',
		tree: 'treeitem',
		treegrid: 'row'
	};

	interface RowState {
		active: boolean;
	}
	interface Props {
		items: T[];
		getKey: (item: T, index: number) => string;
		/** Accessible name of the list. */
		label: string;
		role?: Role;
		/** Row height estimate in px; rows are measured, so two-line rows may differ. */
		estimateSize?: number;
		row: Snippet<[item: T, index: number, state: RowState]>;
		/** Sticky header rendered inside the scroller (a table's column headers). */
		header?: Snippet;
		/** Extra attributes per row (aria-selected, aria-level, ...). */
		itemAttrs?: (item: T, index: number) => Record<string, string | number | boolean | undefined>;
		multiselectable?: boolean;
		/** Total row count when `items` is a window of a larger set (aria-setsize / aria-rowcount). */
		totalCount?: number;
		/** Index of the active row (the one with tabindex 0). Survives prepends via its key. */
		activeIndex?: number;
		onActiveChange?: (index: number, item: T) => void;
		/** Enter, or a click on a row. */
		onActivate?: (index: number, item: T) => void;
		/** `onRow` is false when the key came from a control inside the row (a checkbox, a button). */
		onKey?: (event: KeyboardEvent, ctx: { index: number; item: T; onRow: boolean }) => void;
		/** Enables typeahead: the text a row is matched by. */
		getText?: (item: T) => string;
		/** Cursor paging: rendering the sentinel row calls `onLoadMore` once per `items.length`. */
		hasMore?: boolean;
		loadingMore?: boolean;
		onLoadMore?: () => void;
		/** Text of the sentinel row (e.g. "Loading more…"); announced politely. */
		loadMoreLabel?: string;
		/** When this changes (new query, new sort) the list starts over: first row active, scrolled to top. */
		resetKey?: unknown;
		class?: string;
	}
	let {
		items,
		getKey,
		label,
		role = 'listbox',
		estimateSize = 36,
		row,
		header,
		itemAttrs,
		multiselectable = false,
		totalCount,
		activeIndex = $bindable(0),
		onActiveChange,
		onActivate,
		onKey,
		getText,
		hasMore = false,
		loadingMore = false,
		onLoadMore,
		loadMoreLabel = '',
		resetKey,
		class: className = ''
	}: Props = $props();

	let scroller = $state<HTMLElement>();
	let headerHeight = $state(0);
	const isGrid = $derived(role === 'grid' || role === 'treegrid');
	const count = $derived(items.length + (hasMore ? 1 : 0));

	// --- active row, tracked by key so a prepend cannot move focus to another row -------------
	let activeKey = $state<string | null>(null);
	const indexByKey = $derived.by(() => {
		// eslint-disable-next-line svelte/prefer-svelte-reactivity -- rebuilt by the $derived on every change
		const m = new Map<string, number>();
		items.forEach((it, i) => m.set(getKey(it, i), i));
		return m;
	});
	$effect.pre(() => {
		// Items changed: re-resolve the active row by key; fall back to clamping the index.
		void items;
		untrack(() => {
			const k = activeKey;
			if (k !== null) {
				const at = indexByKey.get(k);
				if (at !== undefined) activeIndex = at;
				else activeIndex = Math.min(activeIndex, Math.max(0, items.length - 1));
			} else activeIndex = Math.min(Math.max(activeIndex, 0), Math.max(0, items.length - 1));
			const item = items[activeIndex];
			if (item !== undefined) activeKey = getKey(item, activeIndex);
		});
	});

	const virt = createVirtualizer<HTMLElement, HTMLElement>({
		count: 0,
		getScrollElement: () => scroller ?? null,
		estimateSize: () => estimateSize,
		overscan: 6
	});

	// Remembered after every render: the first visible row and how far into it the view is.
	let anchor: { key: string; delta: number } | null = null;
	let lastItems: T[] | undefined;
	let lastReset: unknown = untrack(() => resetKey);

	$effect.pre(() => {
		// Keep the active row mounted (so focus survives scrolling away from it) and keyed.
		const active = activeIndex;
		const list = items;
		const margin = headerHeight;
		const size = estimateSize;
		const resetKeyNow = resetKey;
		const total = count;
		void scroller; // re-run once the scroll element is bound so the virtualiser can attach
		// The store notifies on setOptions; read it untracked or this effect re-triggers itself.
		untrack(() => {
			$virt.setOptions({
				count: total,
				estimateSize: () => size,
				scrollMargin: margin,
				scrollPaddingStart: margin,
				getItemKey: (i) => (i < list.length ? getKey(list[i] as T, i) : '__load-more__'),
				rangeExtractor: (range) => {
					// eslint-disable-next-line svelte/prefer-svelte-reactivity -- local to this call
					const out = new Set<number>();
					for (let i = range.startIndex - range.overscan; i <= range.endIndex + range.overscan; i++)
						if (i >= 0 && i < range.count) out.add(i);
					if (active >= 0 && active < range.count) out.add(active);
					return [...out].sort((a, b) => a - b);
				}
			});
			// Rows appeared above the viewport (live inserts): keep what the user is looking at in place.
			const a = list !== lastItems ? anchor : null;
			lastItems = list;
			if (resetKeyNow !== lastReset) {
				lastReset = resetKeyNow;
				activeKey = null;
				activeIndex = 0;
				$virt.scrollToOffset(0);
			} else if (a && scroller && scroller.scrollTop > 0) {
				const at = indexByKey.get(a.key);
				const target = at === undefined ? undefined : $virt.getOffsetForIndex(at, 'start')?.[0];
				if (target !== undefined) $virt.scrollToOffset(target - margin + a.delta);
			}
		});
	});

	$effect(() => {
		const first = rows.find((v) => v.index < items.length);
		if (!first || !scroller) return;
		anchor = {
			key: getKey(items[first.index] as T, first.index),
			delta: scroller.scrollTop - (first.start - headerHeight)
		};
	});

	// --- load more ---------------------------------------------------------------------------
	let requestedAt = -1;
	$effect(() => {
		if (!hasMore || loadingMore || !onLoadMore) return;
		const sentinelShown = $virt.getVirtualItems().some((v) => v.index >= items.length);
		if (sentinelShown && requestedAt !== items.length) {
			requestedAt = items.length;
			untrack(onLoadMore);
		}
	});

	// --- keyboard ----------------------------------------------------------------------------
	function setActive(index: number, focus = true) {
		if (!items.length) return;
		const i = Math.min(Math.max(index, 0), items.length - 1);
		const item = items[i] as T;
		activeIndex = i;
		activeKey = getKey(item, i);
		onActiveChange?.(i, item);
		$virt.scrollToIndex(i, { align: 'auto' });
		if (focus) queueMicrotask(() => focusRow(i));
	}
	function focusRow(i: number) {
		const el = scroller?.querySelector<HTMLElement>(`[data-index="${i}"]`);
		el?.focus({ preventScroll: true });
	}
	export function focusActive() {
		focusRow(activeIndex);
	}
	export function move(delta: number) {
		setActive(activeIndex + delta);
	}
	export function moveTo(index: number) {
		setActive(index);
	}

	let typed = '';
	let typedAt = 0;
	function typeahead(char: string): boolean {
		if (!getText) return false;
		const now = Date.now();
		typed = now - typedAt > 700 ? char : typed + char;
		typedAt = now;
		const needle = typed.toLowerCase();
		const same = typed.length > 1 && [...typed].every((c) => c === typed[0]);
		const from = same ? activeIndex + 1 : activeIndex + (typed.length > 1 ? 0 : 1);
		const n = items.length;
		for (let step = 0; step < n; step++) {
			const i = (from + step) % n;
			const text = getText(items[i] as T).toLowerCase();
			if (text.startsWith(same ? needle[0]! : needle)) {
				setActive(i);
				return true;
			}
		}
		return false;
	}

	function pageSize() {
		return Math.max(1, Math.floor((scroller?.clientHeight ?? 0) / estimateSize) - 1);
	}

	function onkeydown(event: KeyboardEvent) {
		const rowEl = (event.target as HTMLElement).closest<HTMLElement>('[data-index]');
		if (!rowEl || !scroller?.contains(rowEl)) return;
		const index = Number(rowEl.dataset.index);
		const item = items[index];
		if (item === undefined) return;
		const onRow = event.target === rowEl;
		onKey?.(event, { index, item, onRow });
		if (event.defaultPrevented || !onRow) return;
		const k = event.key;
		const handled =
			k === 'ArrowDown'
				? (setActive(index + 1), true)
				: k === 'ArrowUp'
					? (setActive(index - 1), true)
					: k === 'Home'
						? (setActive(0), true)
						: k === 'End'
							? (setActive(items.length - 1), true)
							: k === 'PageDown'
								? (setActive(index + pageSize()), true)
								: k === 'PageUp'
									? (setActive(index - pageSize()), true)
									: k === 'Enter'
										? (onActivate?.(index, item), true)
										: k.length === 1 &&
											  !event.ctrlKey &&
											  !event.metaKey &&
											  !event.altKey &&
											  k !== ' '
											? typeahead(k)
											: false;
		if (handled) event.preventDefault();
	}

	function onclick(event: MouseEvent) {
		const rowEl = (event.target as HTMLElement).closest<HTMLElement>('[data-index]');
		if (!rowEl) return;
		const index = Number(rowEl.dataset.index);
		const item = items[index];
		if (item === undefined) return;
		setActive(index, false);
		onActivate?.(index, item);
	}

	function measure(node: HTMLElement) {
		$virt.measureElement(node);
	}

	const total = $derived(totalCount ?? items.length);
	const rows = $derived($virt.getVirtualItems() as VirtualItem[]);
</script>

<div class="relative h-full {className}">
	<div
		bind:this={scroller}
		{role}
		aria-label={label}
		aria-multiselectable={multiselectable || undefined}
		aria-rowcount={isGrid ? total + (header ? 1 : 0) : undefined}
		{onkeydown}
		{onclick}
		class="h-full overflow-auto"
	>
		{#if header}
			<div bind:offsetHeight={headerHeight} class="sticky top-0 z-10 bg-base-100">
				{@render header()}
			</div>
		{/if}
		<div role="presentation" class="relative w-full" style:height="{$virt.getTotalSize()}px">
			{#each rows as v (v.key)}
				{#if v.index < items.length}
					{@const item = items[v.index] as T}
					{@const active = v.index === activeIndex}
					<div
						use:measure
						data-index={v.index}
						role={ITEM_ROLE[role]}
						aria-posinset={isGrid ? undefined : v.index + 1}
						aria-setsize={isGrid ? undefined : total}
						aria-rowindex={isGrid ? v.index + 1 + (header ? 1 : 0) : undefined}
						{...itemAttrs?.(item, v.index)}
						{...{ tabindex: active ? 0 : -1 }}
						class="ls-focus absolute inset-x-0 top-0 w-full"
						style:transform="translateY({v.start - headerHeight}px)"
					>
						{@render row(item, v.index, { active })}
					</div>
				{:else}
					<div
						use:measure
						data-index={v.index}
						role="presentation"
						aria-hidden="true"
						class="absolute inset-x-0 top-0 w-full p-2 text-center text-small text-muted-text"
						style:transform="translateY({v.start - headerHeight}px)"
					>
						{loadingMore ? loadMoreLabel : ''}
					</div>
				{/if}
			{/each}
		</div>
	</div>
	<div role="status" class="sr-only">{loadingMore ? loadMoreLabel : ''}</div>
</div>
