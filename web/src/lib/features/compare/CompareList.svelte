<script lang="ts">
	// Narrow layout: attribute-first. One section per attribute, "A · name → value" per torrent.
	import DataTableAlternative from '$lib/charts/DataTableAlternative.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { m } from '$lib/paraglide/messages.js';
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
</script>

<div class="cmp-stack" data-testid="compare-list">
	<section aria-labelledby="compare-torrents-heading">
		<h2 id="compare-torrents-heading" class="cmp-section">{m.compare_torrents()}</h2>
		<ol class="cmp-torrents">
			{#each ts as t, i (t.hash)}
				<li data-testid="compare-column-{i}">
					<p class="cmp-line">
						<span class="cmp-letter" aria-hidden="true">{letter(i)}</span>
						<span class="sr-only">{m.compare_torrent_letter({ letter: letter(i) })}: </span>
						<span class="cmp-name">{headingOf(t)}</span>
					</p>
					{#if t.title !== null && t.title !== t.name}
						<Mono wrap class="cmp-raw">{t.name}</Mono>
					{/if}
					<ColumnActions torrent={t} {onCopy} {onPrefetch} {onRemove} />
				</li>
			{/each}
		</ol>
	</section>

	{#each rows as row (row.id)}
		<section aria-labelledby="compare-attr-{row.id}" data-testid="compare-row-{row.id}">
			<h2 id="compare-attr-{row.id}" class="cmp-section">
				<span class={row.mono ? 'font-mono' : ''}>{row.label}</span>
				{#if row.differs}<Differs />{/if}
			</h2>
			<dl class="cmp-values">
				{#each row.cells as cell, i (cell.testid)}
					<div>
						<dt>{letter(i)} · {headingOf(ts[i]!)}</dt>
						<dd><Cell {cell} max={data.scale_bytes} mono={row.mono} /></dd>
					</div>
				{/each}
			</dl>
			{#if row.id === 'composition'}
				<DataTableAlternative
					caption={m.composition()}
					columns={[
						{ key: 'torrent', label: m.torrent() },
						{ key: 'composition', label: m.composition() }
					]}
					rows={row.cells.map((c, i) => ({
						torrent: `${letter(i)} · ${headingOf(ts[i]!)}`,
						composition: c.composition?.summary ?? m.compare_not_classified_yet()
					}))}
					testid="compare-composition-alt"
				/>
			{/if}
		</section>
	{/each}
</div>

<style>
	.cmp-stack {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 6);
	}
	.cmp-section {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: calc(var(--spacing) * 2);
		margin-bottom: calc(var(--spacing) * 2);
		font-size: var(--text-title);
		line-height: var(--text-title--line-height);
		font-weight: var(--font-weight-semibold);
	}
	.cmp-torrents {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 4);
	}
	.cmp-torrents li {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: calc(var(--spacing) * 1);
		border-bottom: var(--border) solid var(--color-base-300);
		padding-bottom: calc(var(--spacing) * 4);
	}
	.cmp-line {
		font-weight: var(--font-weight-semibold);
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
	.cmp-torrents :global(.cmp-raw) {
		color: var(--color-muted-text);
	}
	.cmp-values {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 2);
	}
	.cmp-values div {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 0.5);
	}
	.cmp-values dt {
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
		color: var(--color-muted-text);
		overflow-wrap: anywhere;
	}
	.cmp-values dd {
		overflow-wrap: anywhere;
	}
</style>
