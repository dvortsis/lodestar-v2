<script lang="ts">
	// The adult-enrichment section (ENR-8): what the matching service says about the scene. The API
	// sends it only to the admin session with "Show adult content" on and omits it otherwise, so this
	// loads on its own and only when the field is there (the drawer's first view is size-limited,
	// §10.1). Every value is untrusted text: rendered as text, never as markup.
	import { m } from '$lib/paraglide/messages.js';
	import { safeLink } from './link';

	import type { TorrentDetail } from '../cache';

	let { detail, level = 3 }: { detail: TorrentDetail; level?: number } = $props();
	// Absent when the API left it out (it did, or this is another torrent now): then nothing shows.
	const enrichment = $derived(detail.adult_enrichment);
	const id = $props.id();
	const link = $derived(safeLink(enrichment?.link));
	const rows = $derived([
		['studio', m.detail_adult_enrichment_studio(), enrichment?.studio],
		['released', m.detail_adult_enrichment_released(), enrichment?.released],
		['scene', m.detail_adult_enrichment_scene(), enrichment?.scene_title]
	] as const);
</script>

{#if enrichment}
	<section
		class="ls-detail-section"
		aria-labelledby="{id}-h"
		data-section="adult-enrichment"
		data-testid="detail-adult-enrichment"
	>
		<div role="heading" aria-level={level} id="{id}-h">{m.detail_adult_enrichment_title()}</div>
		<dl class="text-body">
			{#each rows as [key, label, value] (key)}
				{#if value}
					<div data-testid="detail-adult-{key}">
						<dt class="text-small text-muted-text">{label}</dt>
						<dd>{value}</dd>
					</div>
				{/if}
			{/each}
			{#if enrichment.performers.length > 0}
				<div data-testid="detail-adult-performers">
					<dt class="text-small text-muted-text">{m.detail_adult_enrichment_performers()}</dt>
					<dd>
						<ul>
							{#each enrichment.performers as name, i (i)}<li>{name}</li>{/each}
						</ul>
						{#if enrichment.performers_truncated}
							<p class="text-caption text-muted-text" data-testid="detail-adult-performers-more">
								{m.detail_adult_enrichment_performers_more()}
							</p>
						{/if}
					</dd>
				</div>
			{/if}
		</dl>
		<p class="text-caption text-muted-text" data-testid="detail-adult-source">
			{m.detail_adult_enrichment_source({ source: enrichment.source.name })}
		</p>
		{#if link}
			<!-- eslint-disable svelte/no-navigation-without-resolve -- external https URL, checked by safeLink -->
			<a
				class="btn btn-ghost btn-sm"
				href={link}
				target="_blank"
				rel="noopener noreferrer"
				data-testid="detail-adult-link"
				>{m.detail_adult_enrichment_link()}
				<span class="sr-only">{m.detail_adult_enrichment_link_external()}</span></a
			>
		{/if}
	</section>
{/if}
