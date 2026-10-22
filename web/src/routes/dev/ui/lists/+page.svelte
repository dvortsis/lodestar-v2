<script lang="ts">
	// Test-only showcase of the list primitives (W7cd). Not linked anywhere.
	import { Table, Tree, VirtualList } from '$lib/ui/lists';
	import type { Column, FlatNode, SortState } from '$lib/ui/lists';

	// --- VirtualList with cursor paging ----------------------------------------------------
	let results = $state(
		Array.from({ length: 60 }, (_, i) => ({ id: `r${i}`, name: `Result ${i}` }))
	);
	let loadingMore = $state(false);
	let pages = $state(0);
	function loadMore() {
		loadingMore = true;
		setTimeout(() => {
			const n = results.length;
			results = [
				...results,
				...Array.from({ length: 60 }, (_, i) => ({ id: `r${n + i}`, name: `Result ${n + i}` }))
			];
			pages += 1;
			loadingMore = false;
		}, 50);
	}
	let opened = $state('none');
	let prepended = 0;
	function prepend() {
		const fresh = Array.from({ length: 3 }, (_, i) => ({
			id: `new${prepended}-${i}`,
			name: `New ${prepended}-${i}`
		}));
		prepended += 1;
		results = [...fresh, ...results];
	}

	// --- Table -----------------------------------------------------------------------------
	interface Row {
		id: string;
		name: string;
		size: number;
		seeders: number;
	}
	const base: Row[] = Array.from({ length: 2000 }, (_, i) => ({
		id: `t${i}`,
		name: `Torrent ${String(i).padStart(4, '0')}`,
		size: ((i * 7919) % 1000) + 1,
		seeders: (i * 31) % 97
	}));
	const columns: Column<Row>[] = [
		{
			id: 'name',
			header: 'Name',
			value: (r) => r.name,
			sortable: true,
			width: '3fr',
			required: true
		},
		{ id: 'size', header: 'Size', value: (r) => `${r.size} MiB`, sortable: true, align: 'end' },
		{
			id: 'seeders',
			header: 'Seeders',
			value: (r) => String(r.seeders),
			sortable: true,
			align: 'end'
		}
	];
	let sort = $state<SortState>(null);
	let visible = $state(['name', 'size', 'seeders']);
	let selected = $state(new Set<string>());
	const rows = $derived.by(() => {
		if (!sort) return base;
		const { id, dir } = sort;
		const key = (r: Row) => (id === 'name' ? r.name : id === 'size' ? r.size : r.seeders);
		return [...base].sort(
			(a, b) => (key(a) < key(b) ? -1 : key(a) > key(b) ? 1 : 0) * (dir === 'asc' ? 1 : -1)
		);
	});

	// --- Tree (100k nodes) ------------------------------------------------------------------
	const nodes: FlatNode[] = [];
	for (let a = 0; a < 100; a++) {
		nodes.push({ id: `d${a}`, depth: 0, name: `Season ${a + 1}`, dir: true, size: '10 GiB' });
		for (let b = 0; b < 999; b++)
			nodes.push({
				id: `d${a}/f${b}`,
				depth: 1,
				name: `Episode ${b + 1}.mkv`,
				dir: false,
				size: '10 MiB'
			});
	}
	let expanded = $state(new Set<string>());
	let checked = $state(new Set<string>());
</script>

<main class="flex flex-col gap-8 bg-base-200 p-6 text-base-content">
	<h1 class="text-display font-semibold">List primitives</h1>

	<section class="flex flex-col gap-2" aria-labelledby="h-vl">
		<h2 id="h-vl" class="text-title font-semibold">VirtualList</h2>
		<p class="text-small text-muted-text">
			Opened: <span data-testid="vl-opened">{opened}</span>, pages loaded:
			<span data-testid="vl-pages">{pages}</span>
		</p>
		<button type="button" class="btn btn-sm self-start" onclick={prepend}>Prepend 3 results</button>
		<div class="h-(--rail-width) rounded-box border border-base-300 bg-base-100">
			<VirtualList
				items={results}
				getKey={(r) => r.id}
				label="Results"
				getText={(r) => r.name}
				hasMore={pages < 3}
				{loadingMore}
				onLoadMore={loadMore}
				loadMoreLabel="Loading more results"
				onActivate={(_, r) => (opened = r.name)}
			>
				{#snippet row(r)}
					<div
						class="flex min-h-9 items-center border-b border-base-300 px-3 text-body hover:bg-hover"
					>
						{r.name}
					</div>
				{/snippet}
			</VirtualList>
		</div>
	</section>

	<section class="flex flex-col gap-2" aria-labelledby="h-table">
		<h2 id="h-table" class="text-title font-semibold">Table</h2>
		<p class="text-small text-muted-text">
			Selected: <span data-testid="table-selected">{selected.size}</span>
		</p>
		<div class="h-(--results-min-width) rounded-box border border-base-300 bg-base-100">
			<Table
				{rows}
				{columns}
				getKey={(r) => r.id}
				label="Torrents"
				columnsLabel="Columns"
				{sort}
				onSortChange={(s) => (sort = s)}
				visibleColumns={visible}
				onVisibleColumnsChange={(v) => (visible = v)}
				{selected}
				onSelectedChange={(s) => (selected = s)}
				selectAllLabel="Select all"
			/>
		</div>
	</section>

	<section class="flex flex-col gap-2" aria-labelledby="h-tree">
		<h2 id="h-tree" class="text-title font-semibold">Tree</h2>
		<p class="text-small text-muted-text">
			Checked files: <span data-testid="tree-checked">{checked.size}</span>
		</p>
		<div class="h-(--rail-width) rounded-box border border-base-300 bg-base-100">
			<Tree
				{nodes}
				label="Files"
				{expanded}
				onExpandedChange={(e) => (expanded = e)}
				{checked}
				onCheckedChange={(c) => (checked = c)}
			/>
		</div>
	</section>
</main>
