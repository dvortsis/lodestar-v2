<script lang="ts">
	// Core rules, packs and the locked floor. A switch is a button that starts a change: its label
	// follows the server's `on`, never a local toggle. The floor has no control at all.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { coreSwitch, floorRemoval, packSwitch } from './ops';
	import type { Ask } from './types';
	import type { Overview } from './model.svelte';

	let { o, ask }: { o: Overview; ask: Ask } = $props();
</script>

<section class="flex flex-col gap-3" aria-labelledby="filters-core-h">
	<h2 id="filters-core-h" class="text-label font-semibold">{m.filters_core_heading()}</h2>
	<p class="text-muted-text">{m.filters_core_help()}</p>
	<ul class="flex flex-col gap-2" data-testid="filters-core">
		{#each o.core as r (r.id)}
			<li class="flex flex-wrap items-center gap-3" data-testid="core-{r.id}">
				<Mono>{r.id}</Mono>
				<Badge variant={r.on ? 'success' : 'warning'}
					>{r.on ? m.filters_state_on() : m.filters_state_off()}</Badge
				>
				{#if r.floor}
					<Badge>{m.filters_floor_locked()}</Badge>
				{:else}
					<Button
						dense
						onclick={() =>
							ask(coreSwitch(r.id, !r.on), {
								title: m.filters_core_title({ id: r.id }),
								applyLabel: r.on
									? m.filters_core_apply_off({ id: r.id })
									: m.filters_core_apply_on({ id: r.id })
							})}
						data-testid="core-switch-{r.id}"
						>{r.on
							? m.filters_core_turn_off({ id: r.id })
							: m.filters_core_turn_on({ id: r.id })}</Button
					>
				{/if}
			</li>
		{/each}
	</ul>
	<p class="text-muted-text" data-testid="filters-cs1-count">
		{m.filters_cs1_count({ count: o.cs1_entries })}
	</p>
</section>

{#if o.pending_floor_removals.length}
	<section class="flex flex-col gap-3" aria-labelledby="filters-floor-h">
		<h2 id="filters-floor-h" class="text-label font-semibold">{m.filters_pending_heading()}</h2>
		<p class="text-muted-text">{m.filters_pending_help()}</p>
		<ul class="flex flex-col gap-2">
			{#each o.pending_floor_removals as p (p.source)}
				<li class="flex flex-wrap items-center gap-3" data-testid="pending-{p.source}">
					<Mono wrap>{p.name}</Mono>
					<span>{m.filters_pending_entries({ count: p.entries })}</span>
					<Button
						dense
						onclick={() =>
							ask(floorRemoval(p.source, false), { title: m.filters_pending_keep_title() })}
						>{m.filters_pending_keep()}</Button
					>
					<Button
						dense
						onclick={() =>
							ask(floorRemoval(p.source, true), {
								title: m.filters_pending_remove_title(),
								applyLabel: m.filters_pending_remove()
							})}
						data-testid="pending-remove-{p.source}">{m.filters_pending_remove()}</Button
					>
				</li>
			{/each}
		</ul>
	</section>
{/if}

<section class="flex flex-col gap-3" aria-labelledby="filters-packs-h">
	<h2 id="filters-packs-h" class="text-label font-semibold">{m.filters_packs_heading()}</h2>
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
</section>
