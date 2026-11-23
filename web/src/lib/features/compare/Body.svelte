<script lang="ts">
	// The populated comparison. Loaded lazily, in parallel with the request, so the route's own
	// chunk holds only the states and the data path; every label, formatter and layout is here.
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import CompareList from './CompareList.svelte';
	import CompareTable from './CompareTable.svelte';
	import FileDiff from './FileDiff.svelte';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import { formatCount } from '$lib/format';
	import { buildRows, isSame, type LayoutProps } from './model';

	let { data, columns, onCopy, onPrefetch, onRemove }: LayoutProps & { columns: boolean } =
		$props();
	const rows = $derived(buildRows(data, currentLocale()));
	// Not remembered: it starts off every time (§10.6 and the glossary say nothing to keep it).
	let diffOnly = $state(false);
	// A composition row is compared by its text summary; the bars are deliberately ignored.
	const shown = $derived(diffOnly ? rows.filter((r) => !isSame(r)) : rows);
	const hidden = $derived(rows.length - shown.length);
	const hasAttributes = $derived(rows.some((r) => r.id.startsWith('attr-')));
</script>

{#if !hasAttributes}
	<p class="cmp-note" data-testid="compare-no-attributes">{m.compare_no_attributes()}</p>
{/if}
<div class="cmp-toolbar">
	<label class="inline-flex cursor-pointer items-center gap-2 text-body {TARGET_MIN}">
		<!-- Same markup as ui/fields/Toggle.svelte, which isn't reused: its bind:checked costs 1.3 kB here (33.06 kB with it). -->
		<input
			type="checkbox"
			role="switch"
			class="toggle border-field-border {FOCUS_RING}"
			checked={diffOnly}
			onchange={(e) => (diffOnly = e.currentTarget.checked)}
			data-testid="compare-diff-only"
		/>
		<span>{m.compare_differences_only()}</span>
	</label>
	<p class="cmp-hidden" aria-live="polite" data-testid="compare-hidden-count">
		{#if diffOnly && hidden > 0}
			{hidden === 1
				? m.compare_same_hidden_one()
				: m.compare_same_hidden_many({ count: formatCount(hidden, currentLocale()) })}
		{/if}
	</p>
</div>
{#if columns}
	<CompareTable {data} rows={shown} {onCopy} {onPrefetch} {onRemove} />
{:else}
	<CompareList {data} rows={shown} {onCopy} {onPrefetch} {onRemove} />
{/if}
{#if diffOnly && shown.length === 0}
	<p class="cmp-note" data-testid="compare-all-same">{m.compare_all_same()}</p>
{/if}
<FileDiff {data} />

<style>
	.cmp-toolbar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: calc(var(--spacing) * 3);
		margin-bottom: calc(var(--spacing) * 3);
	}
	.cmp-hidden {
		font-size: var(--text-body);
		line-height: var(--text-body--line-height);
		color: var(--color-muted-text);
	}
	.cmp-note {
		margin-top: calc(var(--spacing) * 3);
		margin-bottom: calc(var(--spacing) * 3);
		font-size: var(--text-body);
		line-height: var(--text-body--line-height);
		color: var(--color-muted-text);
	}
</style>
