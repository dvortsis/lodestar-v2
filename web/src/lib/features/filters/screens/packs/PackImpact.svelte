<script lang="ts">
	// What accepting pack versions does (RP-3's `pack_acceptance`), per rule, inside the change
	// dialog. A rule the server can't vouch for says so plainly as the reason for the password.
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { directionText } from './text';
	import type { PackAcceptance } from './types';

	let { rows }: { rows: PackAcceptance[] } = $props();
	const n = (v: number) => formatCount(v, currentLocale());
	const lines = (r: PackAcceptance): [string, string][] => [
		[m.filters_packs_impact_new(), n(r.newly_held)],
		[m.filters_packs_impact_gone(), n(r.no_longer_held)],
		[m.filters_packs_impact_heavier(), n(r.heavier)],
		[m.filters_packs_impact_lighter(), n(r.lighter)],
		[m.filters_packs_impact_disagree(), n(r.disagreements)],
		[m.filters_packs_impact_review(), n(r.review_quarantine + r.review_tombstones)]
	];
</script>

<ul class="flex flex-col gap-3" data-testid="pack-impact">
	{#each rows as r (r.rule)}
		<li class="flex flex-col gap-1" data-testid="pack-impact-{r.rule}">
			<div class="flex flex-wrap items-center gap-2">
				<Mono>{r.rule}@{r.version}</Mono>
				<span class="text-muted-text">{directionText(r.direction)}</span>
				{#if r.in_doubt}
					<Badge variant="warning"
						><span data-testid="pack-in-doubt">{m.filters_packs_in_doubt()}</span></Badge
					>
				{:else if r.strengthens}
					<Badge variant="success">{m.filters_packs_strengthens()}</Badge>
				{:else}
					<Badge variant="warning">{m.filters_packs_weakens()}</Badge>
				{/if}
			</div>
			<ul class="flex flex-col gap-1 text-small">
				{#each lines(r) as [label, value] (label)}
					<li class="flex flex-wrap gap-2">
						<span>{label}</span> <span class="font-semibold">{value}</span>
					</li>
				{/each}
			</ul>
		</li>
	{/each}
</ul>
<p class="text-small text-muted-text">{m.filters_packs_impact_note()}</p>
