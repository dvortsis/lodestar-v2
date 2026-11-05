<script lang="ts">
	// Core rules, packs and the locked floor. A switch is a button that starts a change: its label
	// follows the server's `on`, never a local toggle. The floor has no control at all.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { coreSwitch, floorRemoval } from './ops';
	import type { Ask } from './types';
	import type { Overview } from './model.svelte';

	let { o, ask }: { o: Overview; ask: Ask } = $props();
</script>

<section class="flex flex-col gap-3" aria-labelledby="filters-core-h">
	<h2 id="filters-core-h" class="text-small font-semibold">{m.filters_core_heading()}</h2>
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
		{m.filters_cs1_count({ count: formatCount(o.cs1_entries, currentLocale()) })}
	</p>
</section>

{#if o.pending_floor_removals.length}
	<section class="flex flex-col gap-3" aria-labelledby="filters-floor-h">
		<h2 id="filters-floor-h" class="text-small font-semibold">{m.filters_pending_heading()}</h2>
		<p class="text-muted-text">{m.filters_pending_help()}</p>
		<ul class="flex flex-col gap-2">
			{#each o.pending_floor_removals as p (p.source)}
				<li class="flex flex-wrap items-center gap-3" data-testid="pending-{p.source}">
					<Mono wrap>{p.name}</Mono>
					<span data-testid="pending-count"
						>{m.filters_pending_entries({ count: formatCount(p.entries, currentLocale()) })}</span
					>
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

<!-- The packs load on demand, with what each hides behind its own door (runner, #469). -->
{#await import('./Packs.svelte') then { default: Packs }}
	<Packs {o} {ask} />
{/await}
