<script lang="ts">
	// The consent-class legend (X-sources-restructure, owner §2.66): each class the rows show as a
	// badge, in the code's words, with what turning such a source on asks of you (§18.7). Open
	// sources carry no badge. Text only: the dialogs themselves are unchanged.
	import { m } from '$lib/paraglide/messages.js';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import { CONSENT_CLASSES, consentHelp, consentName } from './sources';
</script>

<section
	class="flex flex-col gap-2"
	aria-labelledby="sources-legend-h"
	data-testid="sources-legend"
>
	<h2 id="sources-legend-h" class="text-small font-semibold">{m.sources_legend_heading()}</h2>
	<p class="text-small text-muted-text">{m.sources_legend_intro()}</p>
	<dl class="flex flex-col gap-2 text-small">
		{#each CONSENT_CLASSES as c (c)}
			<div class="flex flex-wrap items-baseline gap-x-3 gap-y-1" data-testid="sources-legend-{c}">
				<dt class="font-semibold">
					{#if c === 'none'}{consentName(c)}{:else}<Badge>{consentName(c)}</Badge>{/if}
				</dt>
				<dd class="text-muted-text">{consentHelp(c)}</dd>
			</div>
		{/each}
	</dl>
</section>
