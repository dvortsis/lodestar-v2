<script lang="ts">
	// Seeders and Trackers reporting (§10.4): presets ≥ 1 / ≥ 5 / ≥ 50 from the server's swarm
	// buckets. Until swarm health is in the index (M4) the buckets are null: "—" and why, never
	// zeros. "Include never-scraped" waits for a query clause for it (a follow-up).
	import { DASH, formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { TermFacet } from './facets.svelte';

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
	// `≥1`, `≥5`, `≥50` (and `never_scraped`, not a preset yet).
	const presets = $derived(
		(bucket?.values ?? []).flatMap((v) => {
			const n = /^(?:≥|>=)\s*(\d+)$/.exec(v.value)?.[1];
			return n ? [{ n: Number(n), count: v.count }] : [];
		})
	);
</script>

<section class="fr-group" aria-labelledby="fr-{name}-h" data-testid="facet-{name}">
	<h3 class="fr-title" id="fr-{name}-h">{title}</h3>
	{#if bucket === null}
		<p class="fr-muted" data-testid="facet-{name}-unavailable">
			<span aria-hidden="true">{DASH}</span>
			{m.search_facets_unavailable()}. {m.search_facets_unavailable_swarm()}
		</p>
	{:else}
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
					<span class="fr-count" data-testid="facet-{name}-{p.n}-count"
						>{formatCount({ value: p.count, approx: bucket.approx }, locale)}</span
					>
				</button>
			{/each}
		</div>
	{/if}
</section>
