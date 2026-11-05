<script lang="ts">
	// The rule packs on the Filters screen (§20.6), loaded on demand from its switches so the first
	// view keeps within its budget, with what each pack hides behind its own door (runner, #469).
	// A switch is a button that starts a change: its label follows the server's `on`.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { packSwitch } from './ops';
	import type { Ask } from './types';
	import type { Overview } from './model.svelte';

	let { o, ask }: { o: Overview; ask: Ask } = $props();
	// What each pack hides, loaded the first time it's opened.
	let PackInfo = $state.raw<typeof import('./PackInfo.svelte').default>();
</script>

<section class="flex flex-col gap-3" aria-labelledby="filters-packs-h">
	<h2 id="filters-packs-h" class="text-small font-semibold">{m.filters_packs_heading()}</h2>
	<ul class="flex flex-col gap-2" data-testid="filters-packs">
		{#each o.packs as p (p.id)}
			<li class="flex flex-wrap items-center gap-3" data-testid="pack-{p.id}">
				<Mono>{p.id}</Mono>
				<Badge variant={p.on ? 'success' : 'warning'}
					>{p.on ? m.filters_state_on() : m.filters_state_off()}</Badge
				>
				<Button
					dense
					onclick={() =>
						ask(packSwitch(p.id, !p.on), {
							title: m.filters_pack_title({ id: p.id }),
							applyLabel: p.on
								? m.filters_pack_apply_off({ id: p.id })
								: m.filters_pack_apply_on({ id: p.id })
						})}
					data-testid="pack-switch-{p.id}"
					>{p.on
						? m.filters_pack_turn_off({ id: p.id })
						: m.filters_pack_turn_on({ id: p.id })}</Button
				>
			</li>
		{/each}
	</ul>
	<!-- What each pack hides loads on demand, when this is opened (runner, #469). -->
	<details ontoggle={async () => (PackInfo ??= (await import('./PackInfo.svelte')).default)}>
		<summary class="cursor-pointer text-small" data-testid="packs-about"
			>{m.filters_packs_about()}</summary
		>
		{#if PackInfo}<PackInfo />{/if}
	</details>
</section>
