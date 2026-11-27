<script lang="ts">
	// The crawler runs while the wizard is open, so the later steps show what it has found so far
	// (the `crawler` event, every 5 s). The server's numbers only; unknown is "—" (§10.14).
	import { onMount } from 'svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { events, type EventData } from '$lib/state/events.svelte';

	let live = $state.raw<EventData<'crawler'>>();
	const locale = $derived(currentLocale());
	onMount(() => events.on('crawler', (d) => (live = d)));
</script>

<section
	class="flex flex-col gap-2"
	aria-labelledby="wizard-live-heading"
	data-testid="wizard-live"
>
	<h3 id="wizard-live-heading" class="text-body font-semibold">{m.wizard_live_title()}</h3>
	<dl class="flex flex-wrap gap-6">
		{#each [{ label: m.wizard_live_nodes(), value: live?.dht_nodes, id: 'nodes' }, { label: m.harvested(), value: live?.harvested, id: 'harvested' }, { label: m.wizard_live_fetched(), value: live?.fetched, id: 'fetched' }, { label: m.wizard_live_stored(), value: live?.stored, id: 'stored' }] as item (item.id)}
			<div>
				<dt class="text-small text-muted-text">{item.label}</dt>
				<dd class="text-title font-semibold tabular-nums" data-testid="wizard-live-{item.id}">
					{formatCount(item.value, locale)}
				</dd>
			</div>
		{/each}
	</dl>
	<p class="text-small text-muted-text">{m.wizard_live_note()}</p>
</section>
