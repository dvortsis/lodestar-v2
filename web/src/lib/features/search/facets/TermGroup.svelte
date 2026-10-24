<script lang="ts">
	// One term facet (§10.4): its values with the server's disjunctive counts. A value is selected
	// with its checkbox and excluded with the minus button; a selected value shows four ways (the
	// box, medium weight, the value text, and its chip in the strip). Long lists ("Show more") get
	// a search box over the values listed.
	import Minus from '~icons/ph/minus-circle';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import type { TermFacet } from './facets.svelte';

	interface Props {
		/** The server's name (`res`), for test ids. */
		name: string;
		title: string;
		facet: TermFacet;
		/** The value's label (`movie` → "Movies"). */
		labelOf?: (value: string) => string;
		/** "Show more" is on: the server lists up to 100 values. */
		more?: boolean;
		/** Offer "Show more" when the server has more values than it listed. */
		onmore?: (on: boolean) => void;
		/** Exclusion has no meaning for a yes/no flag (excluding one selects the other). */
		excludable?: boolean;
		/** Values the query sets in clauses a click can't rewrite (`fixedValues`): disabled. */
		fixed?: ReadonlySet<string>;
		onvalue: (value: string, how: 'include' | 'exclude') => void;
		onclear: () => void;
	}
	let {
		name,
		title,
		facet,
		labelOf = (v) => v,
		more = false,
		onmore,
		excludable = true,
		fixed = new Set(),
		onvalue,
		onclear
	}: Props = $props();

	const locale = $derived(currentLocale());
	const id = $derived(`fr-${name}`);
	const count = (n: number) => formatCount({ value: n, approx: facet.approx }, locale);
	const active = $derived(facet.values.some((v) => v.selected || v.excluded));

	let find = $state('');
	const shown = $derived(
		more && find.trim()
			? facet.values.filter((v) =>
					labelOf(v.value).toLowerCase().includes(find.trim().toLowerCase())
				)
			: facet.values
	);
	const hasMore = $derived(!!onmore && facet.distinct > facet.values.length);
	const anyFixed = $derived(shown.some((v) => fixed.has(v.value)));
</script>

<section class="fr-group" aria-labelledby="{id}-h" data-testid="facet-{name}">
	<div class="fr-head">
		<h3 class="fr-title" id="{id}-h">{title}</h3>
		{#if active}
			<button
				type="button"
				class="fr-text-button"
				data-testid="facet-{name}-clear"
				onclick={onclear}>{m.search_facets_clear({ facet: title })}</button
			>
		{/if}
	</div>
	{#if more}
		<input
			class="fr-find"
			type="search"
			aria-label={m.search_facets_find()}
			placeholder={m.search_facets_find()}
			bind:value={find}
			data-testid="facet-{name}-find"
		/>
	{/if}
	{#if anyFixed}
		<p class="fr-muted" id="{id}-fixed" data-testid="facet-{name}-fixed">
			{m.search_facets_fixed()}
		</p>
	{/if}
	<ul class="fr-list" aria-labelledby="{id}-h">
		{#each shown as v (v.value)}
			{@const label = labelOf(v.value)}
			{@const locked = fixed.has(v.value)}
			<li class="fr-value" class:fr-selected={v.selected} class:fr-excluded={v.excluded}>
				<span class="fr-label">
					<Checkbox
						label={v.excluded ? `${label} (${m.search_facets_excluded()})` : label}
						checked={v.selected}
						disabled={locked}
						aria-describedby={locked ? `${id}-fixed` : undefined}
						onchange={() => onvalue(v.value, 'include')}
						data-testid="facet-{name}-{v.value}"
					/>
				</span>
				<span class="fr-count" data-testid="facet-{name}-{v.value}-count"
					><span class="sr-only"
						>{v.count === 1 && !facet.approx
							? m.search_facets_count_one()
							: m.search_facets_count({ count: count(v.count) })}</span
					><span aria-hidden="true">{count(v.count)}</span></span
				>
				{#if excludable}
					<button
						type="button"
						class="fr-icon-button"
						aria-label={m.search_facets_exclude({ value: label })}
						aria-pressed={v.excluded}
						disabled={locked}
						aria-describedby={locked ? `${id}-fixed` : undefined}
						data-testid="facet-{name}-{v.value}-exclude"
						onclick={() => onvalue(v.value, 'exclude')}
					>
						<Minus class="fr-note-icon" aria-hidden="true" />
					</button>
				{/if}
			</li>
		{:else}
			{#if find.trim()}<li class="fr-muted">{m.search_facets_find_none()}</li>{/if}
		{/each}
	</ul>
	{#if facet.other > 0 && !more}
		<p class="fr-muted" data-testid="facet-{name}-other">
			{m.search_facets_other({ count: count(facet.other) })}
		</p>
	{/if}
	{#if hasMore || more}
		<button
			type="button"
			class="fr-text-button"
			aria-expanded={more}
			data-testid="facet-{name}-more"
			onclick={() => {
				find = '';
				onmore?.(!more);
			}}>{more ? m.search_facets_show_fewer() : m.search_facets_show_more()}</button
		>
	{/if}
</section>
