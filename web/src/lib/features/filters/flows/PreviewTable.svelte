<script lang="ts" module>
	import type { Preview } from './types';

	export interface PreviewColumn {
		/** Part of each value's `data-testid`: `<testid>-<key>-<field>`. */
		key: string;
		label: string;
		preview: Preview;
	}

	/** The rows, in reading order. Each is its own number, never a sum of others. */
	export const PREVIEW_ROWS = [
		'quarantine',
		'tombstones',
		'stored',
		'removed_from_floor',
		'caught_30d',
		'core_caught_30d',
		'per_day'
	] as const;
</script>

<script lang="ts">
	// Every `Preview` field on its own labelled row (§20.10's exposure summary), one column per
	// preview. Counts go through `src/lib/format`; `≈` only when the server marks the preview an
	// estimate.
	import { formatCount, formatRate } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';

	interface Props {
		caption: string;
		columns: PreviewColumn[];
		testid: string;
		/** Show the column headings (more than one column, or a column with its own name). */
		headed?: boolean;
	}

	let { caption, columns, testid, headed = columns.length > 1 }: Props = $props();

	const labels: Record<(typeof PREVIEW_ROWS)[number], () => string> = {
		quarantine: m.filters_flows_row_quarantine,
		tombstones: m.filters_flows_row_tombstones,
		stored: m.filters_flows_row_stored,
		removed_from_floor: m.filters_flows_row_removed_from_floor,
		caught_30d: m.filters_flows_row_caught_30d,
		core_caught_30d: m.filters_flows_row_core_caught_30d,
		per_day: m.filters_flows_row_per_day
	};

	function value(p: Preview, row: (typeof PREVIEW_ROWS)[number]): string {
		const v = { value: p[row], approx: p.estimate };
		return row === 'per_day'
			? formatRate(v, currentLocale(), 'day')
			: formatCount(v, currentLocale());
	}

	const estimated = $derived(columns.some((c) => c.preview.estimate));
	const single = $derived(columns.length === 1 ? `${testid}-` : null);
</script>

<table class="preview w-full text-body">
	<caption class="text-start font-medium">{caption}</caption>
	{#if headed}
		<thead>
			<tr>
				<td></td>
				{#each columns as c (c.key)}
					<th scope="col" class="text-end font-medium">{c.label}</th>
				{/each}
			</tr>
		</thead>
	{/if}
	<tbody>
		{#each PREVIEW_ROWS as row (row)}
			<tr class="border-t border-base-300">
				<th scope="row" class="text-start font-normal text-muted-text">{labels[row]()}</th>
				{#each columns as c (c.key)}
					<td
						class="text-end tabular-nums"
						data-testid={single ? `${single}${row}` : `${testid}-${c.key}-${row}`}
					>
						{value(c.preview, row)}
					</td>
				{/each}
			</tr>
		{/each}
	</tbody>
</table>
{#if estimated}
	<p class="text-small text-muted-text" data-testid="{testid}-estimate">
		{m.filters_flows_estimate_note()}
	</p>
{/if}

<style>
	.preview :global(:is(th, td)) {
		padding-block: var(--spacing);
		padding-inline-start: calc(var(--spacing) * 2);
	}
	.preview :global(:is(th, td):first-child) {
		padding-inline-start: 0;
	}
</style>
