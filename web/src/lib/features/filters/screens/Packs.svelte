<script lang="ts">
	// The rule packs on the Filters screen (§20.6), loaded on demand from its switches so the first
	// view keeps within its budget, with what each pack hides behind its own door (runner, #469).
	// A switch is a button that starts a change: its label follows the server's `on`. Each pack shows
	// its plain name and what it hides (I5), so the separate "What each pack hides" door is gone.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import { nameOf, textOf } from './names.svelte';
	import { packSwitch } from './ops';
	import RuleLabel from './RuleLabel.svelte';
	import type { Ask } from './types';
	import type { Overview } from './model.svelte';

	let { o, ask }: { o: Overview; ask: Ask } = $props();
</script>

<section class="flex flex-col gap-3" aria-labelledby="filters-packs-h">
	<h2 id="filters-packs-h" class="text-small font-semibold">{m.filters_packs_heading()}</h2>
	<ul class="flex flex-col gap-2" data-testid="filters-packs">
		{#each o.packs as p (p.id)}
			{@const name = nameOf(p.id)}
			<li class="flex flex-wrap items-center gap-3" data-testid="pack-{p.id}">
				<RuleLabel id={p.id} />
				<Badge variant={p.on ? 'success' : 'warning'}
					>{p.on ? m.filters_state_on() : m.filters_state_off()}</Badge
				>
				<Button
					dense
					onclick={() =>
						ask(packSwitch(p.id, !p.on), {
							title: m.filters_pack_title({ name }),
							description: textOf(p.id) ?? undefined,
							applyLabel: p.on
								? m.filters_pack_apply_off({ name })
								: m.filters_pack_apply_on({ name })
						})}
					data-testid="pack-switch-{p.id}"
					>{p.on ? m.filters_pack_turn_off({ name }) : m.filters_pack_turn_on({ name })}</Button
				>
			</li>
		{/each}
	</ul>
</section>
<!-- The per-pack panels (RP-6) load with this part, never with the first view. -->
{#await import('./packs/PacksScreen.svelte') then { default: PackPanels }}
	<PackPanels {o} {ask} />
{:catch}
	<p class="text-muted-text" data-testid="packs-part-failed">{m.filters_packs_part_failed()}</p>
{/await}
