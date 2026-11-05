<script lang="ts">
	// What each rule pack hides (§20.6), loaded on demand when "What each pack hides" is opened, so
	// the descriptions stay out of the Filters screen's first view (runner, #469). The
	// unauthorised-copy pack also says it can't know who holds the rights and that Always allow is
	// the way back (owner §2.39).
	import { m } from '$lib/paraglide/messages.js';
	import Mono from '$lib/ui/labels/Mono.svelte';

	const packs: [string, () => string][] = [
		['malware', () => m.filters_pack_info_malware()],
		['lures', () => m.filters_pack_info_lures()],
		['fakes', () => m.filters_pack_info_fakes()],
		['spam', () => m.filters_pack_info_spam()],
		['scams', () => m.filters_pack_info_scams()],
		['unauthorised-copy', () => m.filters_pack_info_unauthorised_copy()]
	];
</script>

<dl class="mt-2 flex flex-col gap-2 text-small" data-testid="packs-info">
	{#each packs as [id, text] (id)}
		<div>
			<dt><Mono>{id}</Mono></dt>
			<dd class="text-muted-text" data-testid="pack-info-{id}">{text()}</dd>
		</div>
	{/each}
</dl>
