<script lang="ts">
	import Table from '../Table.svelte';
	import type { Column, SortState } from '../table-types';
	interface Row {
		id: string;
		name: string;
		size: string;
		seeders: string;
	}
	let { rows, onSelected }: { rows: Row[]; onSelected?: (s: Set<string>) => void } = $props();
	const columns: Column<Row>[] = [
		{
			id: 'name',
			header: 'Name',
			value: (r) => r.name,
			sortable: true,
			width: '2fr',
			required: true
		},
		{ id: 'size', header: 'Size', value: (r) => r.size, sortable: true, align: 'end' },
		{ id: 'seeders', header: 'Seeders', value: (r) => r.seeders, align: 'end' }
	];
	let sort = $state<SortState>(null);
	let visible = $state(['name', 'size', 'seeders']);
	let selected = $state(new Set<string>());
</script>

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
	onSelectedChange={(s) => {
		selected = s;
		onSelected?.(s);
	}}
	selectAllLabel="Select all"
	selectRowLabel={(r) => `Select ${r.name}`}
/>
<span data-testid="sort">{sort ? `${sort.id}:${sort.dir}` : 'none'}</span>
<span data-testid="selected">{[...selected].join(',')}</span>
