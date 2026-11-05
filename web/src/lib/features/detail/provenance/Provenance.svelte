<script lang="ts">
	// "Found by: DHT · Nyaa" and, for a source whose claims differ from the verified metadata, what
	// differs (§18.8). Private sources arrive only for the admin session (marked "private", icon +
	// text); for anyone else the server sets `withheld`, and the line says only that some are not
	// shown, naming nothing. A claimed name differs byte for byte, so it is information, not a
	// warning.
	import Lock from '~icons/ph/lock-simple';
	import { formatBytes, formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { shortName } from '../../search/unverified/source';
	import UnverifiedBadge from '../../search/unverified/UnverifiedBadge.svelte';
	import type { Provenance } from './types';

	let { provenance, level = 3 }: { provenance: Provenance; level?: number } = $props();
	const locale = $derived(currentLocale());
	const differing = $derived(
		provenance.sources.filter(
			(s) => s.claim && (s.claim.name || s.claim.size || s.claim.file_count)
		)
	);
</script>

{#if provenance.sources.length > 0}
	<p class="text-body" data-testid="detail-found-by">
		<span class="text-muted-text">{m.detail_provenance_found_by()}</span>
		{#each provenance.sources as s, i (s.id)}
			{#if i > 0}<span aria-hidden="true">&ensp;·&ensp;</span>{/if}<span
				data-testid="detail-source-{s.plugin}"
				>{shortName(s.name)}{#if s.private}
					<Lock class="inline size-4" aria-hidden="true" />
					<span class="text-caption">{m.detail_provenance_private()}</span>{/if}</span
			>
		{/each}
	</p>
{/if}
{#if provenance.withheld}
	<p class="text-caption text-muted-text" data-testid="detail-sources-withheld">
		<Lock class="inline size-4" aria-hidden="true" />
		{m.detail_provenance_withheld()}
	</p>
{/if}
{#if !provenance.verified}
	<p><UnverifiedBadge /></p>
{/if}
{#if differing.length > 0}
	<section aria-labelledby="prov-diff" data-testid="detail-claim-differences">
		<div role="heading" aria-level={level} id="prov-diff" class="text-body font-medium">
			{m.detail_provenance_differences()}
		</div>
		<ul class="text-body">
			{#each differing as s (s.id)}
				{#if s.claim}
					{@const c = s.claim}
					{#if c.name}
						<li data-testid="detail-claim-name-{s.plugin}">
							{m.detail_provenance_difference_name({
								source: shortName(s.name),
								claimed: c.name.claimed,
								verified: c.name.verified
							})}
						</li>
					{/if}
					{#if c.size}
						<li data-testid="detail-claim-size-{s.plugin}">
							{m.detail_provenance_difference({
								source: shortName(s.name),
								field: m.detail_provenance_field_size(),
								claimed: formatBytes({ value: c.size.claimed }, locale),
								verified: formatBytes({ value: c.size.verified }, locale)
							})}
						</li>
					{/if}
					{#if c.file_count}
						<li data-testid="detail-claim-files-{s.plugin}">
							{m.detail_provenance_difference({
								source: shortName(s.name),
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
{/if}
