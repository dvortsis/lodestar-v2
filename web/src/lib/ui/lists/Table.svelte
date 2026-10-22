<script lang="ts" generics="T">
	// Data table on VirtualList's grid semantics: sortable headers (aria-sort), a sticky header, a
	// column chooser and a checkbox column with shift-range selection. Every piece of state
	// (sort, visible columns, selection) is a controlled prop; the table never stores anything.
	import IconCaretUp from '~icons/ph/caret-up';
	import IconCaretDown from '~icons/ph/caret-down';
	import IconColumns from '~icons/ph/columns';
	import Popover from '../overlays/Popover.svelte';
	import VirtualList from './VirtualList.svelte';
	import type { Column, SortState } from './table-types';
	import './lists.css';

	interface Props {
		rows: T[];
		columns: Column<T>[];
		getKey: (row: T) => string;
		label: string;
		/** Ids of the shown columns, in order. Defaults to all. */
		visibleColumns?: string[];
		onVisibleColumnsChange?: (ids: string[]) => void;
		/** Label of the chooser button (e.g. "Columns"). Omit to hide the chooser. */
		columnsLabel?: string;
		sort?: SortState;
		onSortChange?: (sort: SortState) => void;
		/** Selection by row key. Omit `onSelectedChange` for a table without a checkbox column. */
		selected?: ReadonlySet<string>;
		onSelectedChange?: (selected: Set<string>) => void;
		selectAllLabel?: string;
		selectRowLabel?: (row: T) => string;
		/** Row height in px (compact density: 36). */
		rowHeight?: number;
		onActivate?: (row: T, index: number) => void;
		hasMore?: boolean;
		loadingMore?: boolean;
		onLoadMore?: () => void;
		loadMoreLabel?: string;
		class?: string;
	}
	let {
		rows,
		columns,
		getKey,
		label,
		visibleColumns,
		onVisibleColumnsChange,
		columnsLabel,
		sort = null,
		onSortChange,
		selected,
		onSelectedChange,
		selectAllLabel = '',
		selectRowLabel,
		rowHeight = 36,
		onActivate,
		hasMore,
		loadingMore,
		onLoadMore,
		loadMoreLabel,
		class: className = ''
	}: Props = $props();

	const shown = $derived(
		visibleColumns
			? (visibleColumns
					.map((id) => columns.find((c) => c.id === id))
					.filter(Boolean) as Column<T>[])
			: columns
	);
	const selectable = $derived(!!onSelectedChange);
	const tracks = $derived(
		`${selectable ? 'var(--control-height-dense) ' : ''}${shown.map((c) => c.width ?? '1fr').join(' ')}`
	);

	function nextSort(id: string): SortState {
		if (sort?.id !== id) return { id, dir: 'asc' };
		return sort.dir === 'asc' ? { id, dir: 'desc' } : null;
	}

	// --- selection --------------------------------------------------------------------------
	let anchor = -1;
	const allSelected = $derived(rows.length > 0 && rows.every((r) => selected?.has(getKey(r))));
	const someSelected = $derived(!allSelected && rows.some((r) => selected?.has(getKey(r))));

	function toggleRow(index: number, shift: boolean) {
		// eslint-disable-next-line svelte/prefer-svelte-reactivity -- a plain copy of controlled state, never mutated after the callback
		const next = new Set(selected);
		const row = rows[index] as T;
		const on = !next.has(getKey(row));
		const [from, to] =
			shift && anchor >= 0 ? [anchor, index].sort((a, b) => a - b) : [index, index];
		for (let i = from as number; i <= (to as number); i++) {
			const k = getKey(rows[i] as T);
			if (on) next.add(k);
			else next.delete(k);
		}
		anchor = index;
		onSelectedChange?.(next);
	}
	function toggleAll() {
		onSelectedChange?.(allSelected ? new Set() : new Set(rows.map(getKey)));
	}
	function indeterminate(node: HTMLInputElement, value: boolean) {
		node.indeterminate = value;
		return {
			update(v: boolean) {
				node.indeterminate = v;
			}
		};
	}

	function toggleColumn(id: string, on: boolean) {
		const current = visibleColumns ?? columns.map((c) => c.id);
		const ids = on
			? columns.map((c) => c.id).filter((c) => c === id || current.includes(c))
			: current.filter((c) => c !== id);
		onVisibleColumnsChange?.(ids);
	}

	function onKey(event: KeyboardEvent, ctx: { index: number; onRow: boolean }) {
		if (event.key === ' ' && selectable && ctx.onRow) {
			event.preventDefault();
			toggleRow(ctx.index, event.shiftKey);
		}
	}
</script>

<div class="flex h-full flex-col {className}">
	{#if columnsLabel && onVisibleColumnsChange}
		<div class="flex justify-end p-2">
			<Popover label={columnsLabel}>
				{#snippet trigger({ props })}
					<button
						{...props}
						type="button"
						class="ls-focus btn btn-sm gap-2 btn-ghost"
						data-testid="column-chooser"
					>
						<IconColumns class="size-(--icon-size-row)" aria-hidden="true" />
						{columnsLabel}
					</button>
				{/snippet}
				<ul class="flex flex-col gap-1">
					{#each columns as col (col.id)}
						<li>
							<label class="flex min-h-(--control-height-dense) items-center gap-2 text-body">
								<input
									type="checkbox"
									class="ls-focus checkbox checkbox-sm"
									checked={(visibleColumns ?? columns.map((c) => c.id)).includes(col.id)}
									disabled={col.required}
									onchange={(e) => toggleColumn(col.id, e.currentTarget.checked)}
								/>
								{col.header}
							</label>
						</li>
					{/each}
				</ul>
			</Popover>
		</div>
	{/if}

	<VirtualList
		items={rows}
		getKey={(r) => getKey(r)}
		{label}
		role="grid"
		estimateSize={rowHeight}
		multiselectable={selectable}
		{hasMore}
		{loadingMore}
		{onLoadMore}
		{loadMoreLabel}
		{onKey}
		resetKey={sort ? `${sort.id}:${sort.dir}` : ''}
		onActivate={(i, r) => onActivate?.(r, i)}
		itemAttrs={(r) => ({
			'aria-selected': selectable ? !!selected?.has(getKey(r)) : undefined
		})}
		class="min-h-0 flex-1"
	>
		{#snippet header()}
			<div
				role="row"
				aria-rowindex={1}
				class="grid items-center border-b border-base-300 text-small font-medium text-muted-text"
				style:grid-template-columns={tracks}
				style:min-height="{rowHeight}px"
			>
				{#if selectable}
					<div role="columnheader" class="flex items-center justify-center">
						<input
							type="checkbox"
							class="ls-focus checkbox checkbox-sm"
							aria-label={selectAllLabel}
							checked={allSelected}
							use:indeterminate={someSelected}
							onchange={toggleAll}
						/>
					</div>
				{/if}
				{#each shown as col (col.id)}
					<div
						role="columnheader"
						aria-sort={col.sortable
							? sort?.id === col.id
								? sort.dir === 'asc'
									? 'ascending'
									: 'descending'
								: 'none'
							: undefined}
						class="px-2 {col.align === 'end' ? 'text-end' : 'text-start'}"
					>
						{#if col.sortable}
							<button
								type="button"
								onclick={() => onSortChange?.(nextSort(col.id))}
								class="ls-focus inline-flex min-h-(--control-height-dense) items-center gap-1 font-medium pointer-coarse:min-h-(--target-min)"
							>
								{col.header}
								{#if sort?.id === col.id}
									{#if sort.dir === 'asc'}
										<IconCaretUp class="size-(--icon-size-row)" aria-hidden="true" />
									{:else}
										<IconCaretDown class="size-(--icon-size-row)" aria-hidden="true" />
									{/if}
								{/if}
							</button>
						{:else}
							{col.header}
						{/if}
					</div>
				{/each}
			</div>
		{/snippet}
		{#snippet row(r: T, index: number)}
			<div
				class="grid items-center border-b border-base-300 text-body hover:bg-hover"
				class:bg-selected={selectable && selected?.has(getKey(r))}
				style:grid-template-columns={tracks}
				style:min-height="{rowHeight}px"
			>
				{#if selectable}
					<div role="gridcell" class="flex items-center justify-center">
						<input
							type="checkbox"
							tabindex="-1"
							class="ls-focus checkbox checkbox-sm"
							aria-label={selectRowLabel?.(r)}
							checked={!!selected?.has(getKey(r))}
							onclick={(e) => {
								e.stopPropagation();
								toggleRow(index, e.shiftKey);
							}}
						/>
					</div>
				{/if}
				{#each shown as col (col.id)}
					<div
						role="gridcell"
						class="min-w-0 truncate px-2 tabular-nums {col.align === 'end' ? 'text-end' : ''}"
					>
						{#if col.cell}{@render col.cell(r)}{:else}{col.value?.(r) ?? ''}{/if}
					</div>
				{/each}
			</div>
		{/snippet}
	</VirtualList>
</div>
