<script lang="ts">
	// Per-pack panels under the packs list (§20.7, RP-6): versions, "What changed", corpus status,
	// pending versions and the override editor. Loaded on demand from the switches so the Filters
	// first view keeps within its budget.
	import { onDestroy, onMount } from 'svelte';
	import { api, isAbort } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import type { Ask } from '../types';
	import type { Overview } from '../model.svelte';
	import PackPanel from './PackPanel.svelte';
	import type { PackCorpus } from './types';

	let { o, ask }: { o: Overview; ask: Ask } = $props();

	let corpus = $state.raw<PackCorpus | null>(null);
	const ctl = new AbortController();
	onMount(async () => {
		try {
			corpus = (await api.get('/filters/fixtures', { signal: ctl.signal })).packs;
		} catch (e) {
			// Unreadable → "not known" on screen, never "passing".
			if (!isAbort(e)) corpus = null;
		}
	});
	onDestroy(() => ctl.abort());
	const pendingFor = (id: string) => (o.pending_pack_versions ?? []).filter((p) => p.pack === id);
</script>

{#if o.packs.length}
	<section
		class="flex flex-col gap-3"
		aria-labelledby="filters-packs-detail-h"
		data-testid="filters-pack-panels"
	>
		<h2 id="filters-packs-detail-h" class="text-small font-semibold">
			{m.filters_packs_detail_heading()}
		</h2>
		{#each o.packs as pack (pack.id)}
			<PackPanel {pack} pending={pendingFor(pack.id)} {corpus} {ask} />
		{/each}
	</section>
{/if}
