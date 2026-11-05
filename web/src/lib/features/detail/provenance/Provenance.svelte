<script lang="ts">
	// "Found by: DHT · Nyaa" (§18.8). Private sources arrive only for the admin session (marked
	// "private"); for anyone else the server sets `withheld`, and a separate note says some details
	// are private, naming nothing. What differs from the verified metadata loads on its own, and
	// only when something does (the drawer's first view is size-limited, §10.1).
	import { m } from '$lib/paraglide/messages.js';
	import type { Provenance } from './types';

	let { provenance, level = 3 }: { provenance: Provenance; level?: number } = $props();
	const short = (name: string) => name.replace(/^Mainline\s+/, '');
	const differing = $derived(
		provenance.sources.filter(
			(s) => s.claim && (s.claim.name || s.claim.size || s.claim.file_count)
		)
	);

	let Differences = $state.raw<typeof import('./Differences.svelte').default>();
	let asked = false;
	$effect(() => {
		if (differing.length && !asked) {
			asked = true;
			void import('./Differences.svelte').then((mod) => (Differences = mod.default));
		}
	});
</script>

{#if provenance.sources.length > 0}
	<p class="text-body" data-testid="detail-found-by">
		<span class="text-muted-text">{m.detail_provenance_found_by()}</span>
		{#each provenance.sources as s, i (s.id)}
			{#if i > 0}<span class="sr-only">, </span><span aria-hidden="true">&ensp;·&ensp;</span
				>{/if}<span data-testid="detail-source-{s.plugin}"
				>{short(s.name)}{#if s.private}
					<span class="text-caption">({m.detail_provenance_private()})</span>{/if}</span
			>
		{/each}
	</p>
{/if}
{#if provenance.withheld}
	<p class="text-caption text-muted-text" data-testid="detail-sources-withheld">
		{m.detail_provenance_withheld()}
	</p>
{/if}
{#if differing.length > 0 && Differences}
	<Differences sources={differing} {level} />
{/if}
