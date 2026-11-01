<script lang="ts">
	// One chart's panel: title, a one-sentence summary built from labels and the time base, the chart,
	// "View as table" and CSV export (§10.9.6). The table is the chart's text alternative; it is
	// read by assistive tech even while hidden.
	import type { Snippet } from 'svelte';
	import DataTableAlternative from '$lib/charts/DataTableAlternative.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { resolve } from '$app/paths';
	import type { Route } from './href';
	import { csvHref } from './links';
	import type { InsightsQuery } from './query';

	interface Props {
		id: string;
		title: string;
		summary: string;
		caption: string;
		columns: { key: string; label: string; numeric?: boolean }[];
		rows: Record<string, string>[];
		/** The export panel name; omit for a panel with no CSV. */
		csv?: string;
		query: InsightsQuery;
		/** Show the table without the toggle (the chart could not load). */
		forceTable?: boolean;
		testid: string;
		children: Snippet;
	}
	let {
		id,
		title,
		summary,
		caption,
		columns,
		rows,
		csv,
		query,
		forceTable = false,
		testid,
		children
	}: Props = $props();

	let shown = $state(false);
	const visible = $derived(shown || forceTable);
</script>

<div class="ins-card" data-testid={testid}>
	<div class="ins-card-head">
		<h3 class="ins-card-title">{title}</h3>
		<div class="ins-actions">
			{#if !forceTable}
				<Button
					variant="quiet"
					dense
					aria-expanded={visible}
					aria-controls="{id}-table"
					data-testid="{testid}-toggle"
					onclick={() => (shown = !shown)}
				>
					{visible ? m.insights_overview_hide_table() : m.insights_overview_view_table()}
				</Button>
			{/if}
			{#if csv}
				<a
					class="ins-link target"
					href={resolve(csvHref(csv, query) as Route)}
					download
					data-testid="{testid}-csv">{m.insights_overview_csv()}</a
				>
			{/if}
		</div>
	</div>
	<p class="ins-summary" data-testid="{testid}-summary">{summary}</p>
	{@render children()}
	<div id="{id}-table">
		<DataTableAlternative {caption} {columns} {rows} {visible} testid="{testid}-table" />
	</div>
</div>
