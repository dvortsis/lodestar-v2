<script lang="ts">
	// Columns layout, from `x-large`: a real table, one column per torrent.
	import DataTableAlternative from '$lib/charts/DataTableAlternative.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { formatCount } from '$lib/format';
	import Cell from './Cell.svelte';
	import ColumnActions from './ColumnActions.svelte';
	import Differs from './Differs.svelte';
	import { headingOf, letter, type CompareResponse, type Row } from './model';

	interface Props {
		data: CompareResponse;
		rows: Row[];
		onCopy: (hash: string) => void;
		onPrefetch: (hash: string) => void;
		onRemove: (hash: string) => void;
	}
	let { data, rows, onCopy, onPrefetch, onRemove }: Props = $props();
	const ts = $derived(data.torrents);
	const composition = $derived(rows.find((r) => r.id === 'composition'));
</script>

<div class="cmp-scroll">
	<table class="cmp-grid" data-testid="compare-table">
		<caption class="sr-only"
			>{m.compare_caption({ count: formatCount(ts.length, currentLocale()) })}</caption
		>
		<thead>
			<tr>
				<th scope="col"><span class="sr-only">{m.compare_attribute()}</span></th>
				{#each ts as t, i (t.hash)}
					<th scope="col" class="cmp-head" data-testid="compare-column-{i}">
						<span class="cmp-letter" aria-hidden="true">{letter(i)}</span>
						<span class="sr-only">{m.compare_torrent_letter({ letter: letter(i) })}: </span>
						<span class="cmp-name">{headingOf(t)}</span>
						{#if t.title !== null && t.title !== t.name}
							<Mono wrap class="cmp-raw">{t.name}</Mono>
						{/if}
					</th>
				{/each}
			</tr>
		</thead>
		<tbody>
			<tr>
				<th scope="row">{m.compare_actions()}</th>
				{#each ts as t (t.hash)}
					<td><ColumnActions torrent={t} {onCopy} {onPrefetch} {onRemove} /></td>
				{/each}
			</tr>
			{#each rows as row (row.id)}
				<tr data-testid="compare-row-{row.id}">
					<th scope="row">
						<span class={row.mono ? 'font-mono' : ''}>{row.label}</span>
						{#if row.differs}<Differs />{/if}
					</th>
					{#each row.cells as cell (cell.testid)}
						<td><Cell {cell} max={data.scale_bytes} mono={row.mono} /></td>
					{/each}
				</tr>
			{/each}
		</tbody>
	</table>
</div>
{#if composition}
	<DataTableAlternative
		caption={m.composition()}
		columns={[
			{ key: 'torrent', label: m.torrent() },
			{ key: 'composition', label: m.composition() }
		]}
		rows={composition.cells.map((c, i) => ({
			torrent: `${letter(i)} · ${headingOf(ts[i]!)}`,
			composition: c.composition?.summary ?? m.compare_not_classified_yet()
		}))}
		testid="compare-composition-alt"
	/>
{/if}

<style>
	.cmp-scroll {
		overflow-x: auto;
	}
	.cmp-grid {
		width: 100%;
		table-layout: fixed;
		border-spacing: 0;
		text-align: left;
		font-size: var(--text-body);
		line-height: var(--text-body--line-height);
	}
	.cmp-grid th,
	.cmp-grid td {
		border-bottom: var(--border) solid var(--color-base-300);
		padding: calc(var(--spacing) * 3);
		vertical-align: top;
		overflow-wrap: anywhere;
	}
	.cmp-grid th {
		font-weight: var(--font-weight-medium);
	}
	.cmp-grid thead th:first-child {
		width: 16%;
	}
	.cmp-grid tbody th {
		color: var(--color-muted-text);
	}
	.cmp-letter {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: calc(var(--spacing) * 6);
		height: calc(var(--spacing) * 6);
		margin-right: calc(var(--spacing) * 2);
		border: var(--border) solid var(--color-field-border);
		border-radius: var(--radius-selector);
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
		font-weight: var(--font-weight-semibold);
	}
	.cmp-name {
		font-weight: var(--font-weight-semibold);
	}
	.cmp-head :global(.cmp-raw) {
		display: block;
		margin-top: calc(var(--spacing) * 1);
		color: var(--color-muted-text);
	}
</style>
