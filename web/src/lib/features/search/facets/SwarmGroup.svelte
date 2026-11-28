<script lang="ts">
	// Seeders and Trackers reporting (§10.4, UI review I6): presets ≥ 1 / ≥ 5 / ≥ 50 (trackers
	// ≥ 1 / ≥ 3) as plain clauses. With the server's swarm buckets each shows its count; without
	// them (null) the presets still work, with no count: never zeros. "Include never-scraped" waits
	// for a query clause for it (a follow-up).
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { TermFacet } from './facets.svelte';
	import { swarmPresets } from './presets';

	interface Props {
		name: 'seeders' | 'trackers';
		title: string;
		bucket: TermFacet | null;
		/** The field's `>=` bound in the query, if any. */
		atLeast: number | null;
		onatleast: (n: number | null) => void;
	}
	let { name, title, bucket, atLeast, onatleast }: Props = $props();

	const locale = $derived(currentLocale());
	const presets = $derived(swarmPresets(name, bucket));
</script>

<section class="fr-group" aria-labelledby="fr-{name}-h" data-testid="facet-{name}">
	<h3 class="fr-title" id="fr-{name}-h">{title}</h3>
	<div class="fr-presets" role="group" aria-label={m.search_facets_presets({ facet: title })}>
		{#each presets as p (p.n)}
			<button
				type="button"
				class="fr-preset"
				aria-pressed={atLeast === p.n}
				data-testid="facet-{name}-{p.n}"
				onclick={() => onatleast(atLeast === p.n ? null : p.n)}
			>
				{m.search_facets_at_least({ count: formatCount(p.n, locale) })}
				{#if p.count !== null && bucket}<span
						class="fr-count"
						data-testid="facet-{name}-{p.n}-count"
						>{formatCount({ value: p.count, approx: bucket.approx }, locale)}</span
					>{/if}
			</button>
		{/each}
	</div>
</section>
