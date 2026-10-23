<script lang="ts">
	// The populated comparison. Loaded lazily, in parallel with the request, so the route's own
	// chunk holds only the states and the data path; every label, formatter and layout is here.
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import CompareList from './CompareList.svelte';
	import CompareTable from './CompareTable.svelte';
	import FileDiff from './FileDiff.svelte';
	import { buildRows, type LayoutProps } from './model';

	let { data, columns, onCopy, onPrefetch, onRemove }: LayoutProps & { columns: boolean } =
		$props();
	const rows = $derived(buildRows(data, currentLocale()));
	const hasAttributes = $derived(rows.some((r) => r.id.startsWith('attr-')));
</script>

{#if !hasAttributes}
	<p class="cmp-note" data-testid="compare-no-attributes">{m.compare_no_attributes()}</p>
{/if}
{#if columns}
	<CompareTable {data} {rows} {onCopy} {onPrefetch} {onRemove} />
{:else}
	<CompareList {data} {rows} {onCopy} {onPrefetch} {onRemove} />
{/if}
<FileDiff {data} />

<style>
	.cmp-note {
		margin-bottom: calc(var(--spacing) * 3);
		font-size: var(--text-body);
		line-height: var(--text-body--line-height);
		color: var(--color-muted-text);
	}
</style>
