<script lang="ts">
	// A histogram facet (§10.4: Discovered, Size): the server's bins drawn as they are (explicit
	// edges, zero baseline, empty bins empty), with a brush by pointer or keys (charts/Histogram).
	// A settled brush replaces the facet's range clause; presets go above it (a snippet).
	import type { Snippet } from 'svelte';
	import Info from '~icons/ph/info';
	import type { BrushRange } from '$lib/charts/brush';
	import Histogram from '$lib/charts/Histogram.svelte';
	import { DASH, formatBytes, formatCount, formatDate } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { brushOfRange, edgesOf, rangeOfBrush } from './bins';
	import type { Histogram as Hist } from './facets.svelte';

	interface Props {
		name: string;
		title: string;
		hist: Hist;
		/** The facet's clause, as inclusive bounds in the histogram's units (null: none). */
		range: { lo: number | null; hi: number | null } | null;
		/** A settled brush: the new bounds, or null for the whole histogram (no clause). */
		onrange: (range: { lo: number | null; hi: number | null } | null) => void;
		presets?: Snippet;
	}
	let { name, title, hist, range, onrange, presets }: Props = $props();

	const locale = $derived(currentLocale());
	// Size's edges are bytes; Discovered's are UTC seconds; Year's are years (`unit: "year"` with
	// small numbers, unlike Discovered's yearly bins in seconds).
	const edgeText = (e: number | null) =>
		e === null
			? DASH
			: hist.unit === 'bytes'
				? formatBytes(e, locale)
				: name === 'year'
					? String(e)
					: formatDate(e, locale);
	const edges = $derived(edgesOf(hist.bins).map(edgeText));
	// The open top bin has no upper edge: the tick under it reads "<last edge> +", not a dash.
	const tickLabels = $derived(
		edges.length > 1 && edgesOf(hist.bins).at(-1) === null
			? [...edges.slice(0, -1), `${edges.at(-2)} +`]
			: edges
	);
	const counts = $derived(hist.bins.map((b) => ({ value: b.count, approx: hist.approx })));
	const brush = $derived(brushOfRange(hist.bins, range));
	const peak = $derived(Math.max(0, ...hist.bins.map((b) => b.count)));
	const summary = $derived(
		m.search_facets_bins_summary({
			bins: formatCount(hist.bins.length, locale),
			from: edges[0] ?? DASH,
			to: edges[edges.length - 1] ?? DASH,
			peak: formatCount({ value: peak, approx: hist.approx }, locale)
		})
	);
	const backfill = $derived(hist.bins.some((b) => b.backfill));

	function commit(next: BrushRange | null) {
		onrange(next ? rangeOfBrush(hist.bins, next, range) : null);
	}
</script>

<section class="fr-group" aria-labelledby="fr-{name}-h" data-testid="facet-{name}">
	<div class="fr-head">
		<h3 class="fr-title" id="fr-{name}-h">{title}</h3>
		{#if range}
			<button
				type="button"
				class="fr-text-button"
				data-testid="facet-{name}-clear"
				onclick={() => onrange(null)}>{m.search_facets_clear({ facet: title })}</button
			>
		{/if}
	</div>
	{@render presets?.()}
	{#if hist.bins.length}
		<Histogram
			{counts}
			{edges}
			{tickLabels}
			label={m.search_facets_histogram({ facet: title, summary })}
			handleLabels={{ from: m.search_facets_from(), to: m.search_facets_to() }}
			selection={brush}
			oncommit={commit}
			testid="facet-{name}-histogram"
		/>
	{/if}
	{#if hist.missing > 0}
		<p class="fr-muted" data-testid="facet-{name}-missing">
			{m.search_facets_missing({
				count: formatCount({ value: hist.missing, approx: hist.approx }, locale)
			})}
		</p>
	{/if}
	{#if backfill}
		<p class="fr-note" data-testid="facet-{name}-backfill">
			<Info class="fr-note-icon" aria-hidden="true" />{m.search_facets_backfill_marker()}
		</p>
	{/if}
</section>
