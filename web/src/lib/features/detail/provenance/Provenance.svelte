<script lang="ts">
	// "Found by: DHT · Nyaa" and, when claims and verified metadata disagree, what differs (§18.8).
	// A source with no name (private, for a non-admin) is not listed: nothing here names it.
	import { m } from '$lib/paraglide/messages.js';
	import UnverifiedBadge from '../../search/unverified/UnverifiedBadge.svelte';
	import type { Provenance } from './types';

	let { provenance }: { provenance: Provenance } = $props();
	const names = $derived(provenance.sources.flatMap((s) => (s.name === null ? [] : [s.name])));
	const label = (field: string) =>
		field === 'size'
			? m.detail_provenance_field_size()
			: field === 'files'
				? m.detail_provenance_field_files()
				: m.detail_provenance_field_name();
</script>

{#if names.length > 0}
	<p class="text-body" data-testid="detail-found-by">
		<span class="text-base-content/70">{m.detail_provenance_found_by()}</span>
		{names.join(' · ')}
	</p>
{/if}
{#if !provenance.verified}
	<p><UnverifiedBadge /></p>
{/if}
{#if provenance.differences.length > 0}
	<section aria-labelledby="prov-diff" data-testid="detail-claim-differences">
		<h3 id="prov-diff" class="text-body font-medium">{m.detail_provenance_differences()}</h3>
		<ul class="text-body">
			{#each provenance.differences as d (d.field)}
				<li>
					{m.detail_provenance_difference({
						field: label(d.field),
						claimed: d.claimed,
						verified: d.verified
					})}
				</li>
			{/each}
		</ul>
	</section>
{/if}
