<script lang="ts">
	// Where a source's claims differ from the verified metadata (§18.8): information, not a warning
	// (a feed's tidied title nearly always differs byte for byte). Loaded only when something differs.
	import { formatBytes, formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { sourceLabel } from '$lib/format/label-source';
	import { m } from '$lib/paraglide/messages.js';
	import type { ProvenanceSource } from './types';

	let { sources, level = 3 }: { sources: ProvenanceSource[]; level?: number } = $props();
	const id = $props.id();
	const locale = $derived(currentLocale());
	// A plugin's name in plain words (X-labels); the DHT is "DHT".
	const short = (s: { plugin: string; name: string }) => sourceLabel(s.plugin, s.name);
</script>

<section aria-labelledby="{id}-h" data-testid="detail-claim-differences">
	<div role="heading" aria-level={level} id="{id}-h" class="text-body font-medium">
		{m.detail_provenance_differences()}
	</div>
	<ul class="text-body">
		{#each sources as s (s.id)}
			{#if s.claim}
				{@const c = s.claim}
				{#if c.name}
					<li data-testid="detail-claim-name-{s.plugin}">
						{m.detail_provenance_difference_name({
							source: short(s),
							claimed: c.name.claimed,
							verified: c.name.verified
						})}
					</li>
				{/if}
				{#if c.size}
					<li data-testid="detail-claim-size-{s.plugin}">
						{m.detail_provenance_difference({
							source: short(s),
							field: m.detail_provenance_field_size(),
							claimed: formatBytes({ value: c.size.claimed }, locale),
							verified: formatBytes({ value: c.size.verified }, locale)
						})}
					</li>
				{/if}
				{#if c.file_count}
					<li data-testid="detail-claim-files-{s.plugin}">
						{m.detail_provenance_difference({
							source: short(s),
							field: m.detail_provenance_field_files(),
							claimed: formatCount({ value: c.file_count.claimed }, locale),
							verified: formatCount({ value: c.file_count.verified }, locale)
						})}
					</li>
				{/if}
			{/if}
		{/each}
	</ul>
</section>
