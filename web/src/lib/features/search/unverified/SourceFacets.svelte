<script lang="ts">
	// The Source facet and the "Show unverified" control with its count, mounted in the facet rail
	// (one line). Counts are the server's; `source` counts are partial while its index field is
	// still being filled in. A private source never arrives by name for a non-admin caller: the
	// server folds it into `other`, which TermGroup shows without naming anything.
	import { m } from '$lib/paraglide/messages.js';
	import type { Ast } from '$lib/state/search.svelte';
	import TermGroup from '../facets/TermGroup.svelte';
	import type { FacetsResponse } from '../facets/facets.svelte';
	import { clearField, fieldOf, fixedValues, toggleValue } from '../facets/toggle';
	import { shortName } from './source';
	import UnverifiedToggle from './UnverifiedToggle.svelte';

	interface Props {
		data: FacetsResponse;
		ast: Ast | null;
		onchange: (next: Ast) => void;
	}
	let { data, ast, onchange }: Props = $props();

	const FIELD = fieldOf('source')!;
	const source = $derived(data.facets.source);
	const unverified = $derived(data.facets.verified?.values.find((v) => v.value === 'no'));
	const nameOf = (id: string) => shortName(data.source_names[id] ?? id);
</script>

{#if source && source.values.length}
	<div data-testid="facet-source-wrap">
		<TermGroup
			name="source"
			title={m.search_unverified_source()}
			facet={source}
			labelOf={nameOf}
			excludable={false}
			fixed={fixedValues(ast, FIELD)}
			onvalue={(v) => onchange(toggleValue(ast, FIELD, v))}
			onclear={() => onchange(clearField(ast, FIELD))}
		/>
		{#if source.partial}
			<p class="fr-muted" data-testid="facet-source-partial">
				{m.search_unverified_partial()}
			</p>
		{/if}
	</div>
{/if}
<UnverifiedToggle
	{ast}
	{onchange}
	count={unverified
		? { value: unverified.count, approx: data.facets.verified?.approx ?? false }
		: null}
/>
