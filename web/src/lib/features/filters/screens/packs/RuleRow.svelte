<script lang="ts">
	// One pack rule: its switch, the action that applies, your override against the base (base beside yours),
	// and the override editor. A paused or changed override is flagged; a paused one has *Confirm
	// again*. Every change goes through the flows' dialog; whether it weakens is the server's call.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Select from '$lib/ui/fields/Select.svelte';
	import type { Ask } from '../types';
	import { confirmOverride, removeOverride, setOverride, switchRule } from './ops';
	import { ACTIONS, actionText } from './text';
	import type { PackRuleState } from './types';

	let { rule: r, ask }: { rule: PackRuleState; ask: Ask } = $props();

	const ov = $derived(r.pack_override);
	let action = $state<string>('');
	let unless = $state('');
	// The editor starts from the stored override, or the base action. It is seeded again only when
	// that stored value changes (compared by value), so a reload never discards an unsaved edit.
	const seed = $derived(JSON.stringify([ov?.action ?? r.base_action, ov?.unless ?? '']));
	$effect(() => {
		const [a, u] = JSON.parse(seed) as [string, string];
		action = (ACTIONS as readonly string[]).includes(a) ? a : 'hide';
		unless = u;
	});
	const options = ACTIONS.map((a) => ({ value: a, label: actionText(a) }));
</script>

<li class="flex flex-col gap-2" data-testid="pack-rule-{r.id}">
	<div class="flex flex-wrap items-center gap-3">
		<Mono>{r.id}@{r.version}</Mono>
		<Badge variant={r.on ? 'success' : 'warning'}
			>{r.on ? m.filters_state_on() : m.filters_state_off()}</Badge
		>
		<Badge>{actionText(r.action)}</Badge>
		{#if r.pending !== null}
			<Badge variant="info"
				><span data-testid="pack-rule-pending-{r.id}"
					>{m.filters_packs_waiting({ version: r.pending })}</span
				></Badge
			>
		{/if}
		<Button
			dense
			onclick={() =>
				ask(switchRule(r.id, !r.on), {
					title: m.filters_pack_title({ id: r.id }),
					applyLabel: r.on
						? m.filters_pack_apply_off({ id: r.id })
						: m.filters_pack_apply_on({ id: r.id })
				})}
			data-testid="pack-rule-switch-{r.id}"
			>{r.on ? m.filters_pack_turn_off({ id: r.id }) : m.filters_pack_turn_on({ id: r.id })}</Button
		>
	</div>

	{#if ov}
		<div class="flex flex-col gap-1 text-small" data-testid="pack-override-{r.id}">
			{#if ov.unreadable}
				<Badge variant="error">{m.filters_packs_ov_unreadable()}</Badge>
				<p class="text-muted-text">{m.filters_packs_ov_unreadable_help()}</p>
			{:else}
				<!-- Base beside yours, as two labelled lines (never colour alone). -->
				<ul class="flex flex-col gap-1" data-testid="pack-vs-{r.id}">
					<li>
						<span class="text-muted-text">{m.filters_packs_vs_base()}</span>
						{actionText(r.base_action)}
					</li>
					<li>
						<span class="text-muted-text">{m.filters_packs_vs_yours()}</span>
						{actionText(ov.action)}
						{#if ov.unless}· {m.filters_packs_vs_unless()} <Mono wrap>{ov.unless}</Mono>{/if}
					</li>
				</ul>
				{#if ov.paused}
					<Badge variant="warning"
						><span data-testid="pack-paused-{r.id}">{m.filters_packs_ov_paused()}</span></Badge
					>
					<p class="text-muted-text">{m.filters_packs_ov_paused_help()}</p>
				{:else if r.base_changed || ov.changed}
					<Badge variant="info"
						><span data-testid="pack-changed-{r.id}">{m.filters_packs_ov_changed()}</span></Badge
					>
				{/if}
				{#if ov.paused || r.base_changed || ov.changed}
					<div>
						<Button
							dense
							onclick={() =>
								ask(confirmOverride(r.id), {
									title: m.filters_packs_confirm_title({ id: r.id }),
									applyLabel: m.filters_packs_confirm_apply({ id: r.id })
								})}
							data-testid="pack-confirm-{r.id}">{m.filters_packs_confirm({ id: r.id })}</Button
						>
					</div>
				{/if}
			{/if}
		</div>
	{/if}

	<details>
		<summary class="cursor-pointer text-small" data-testid="pack-edit-{r.id}"
			>{m.filters_packs_edit({ id: r.id })}</summary
		>
		<form
			class="mt-2 flex flex-col gap-3"
			onsubmit={(e) => {
				e.preventDefault();
				ask(setOverride(r.id, action, unless.trim()), {
					title: m.filters_packs_override_title({ id: r.id }),
					applyLabel: m.filters_packs_override_apply({ id: r.id })
				});
			}}
		>
			<Field label={m.filters_packs_override_action()}>
				{#snippet children(props)}
					<Select {...props} bind:value={action} {options} data-testid="pack-action-{r.id}" />
				{/snippet}
			</Field>
			<Field
				label={m.filters_packs_override_unless()}
				help={m.filters_packs_override_unless_help()}
			>
				{#snippet children(props)}
					<Input {...props} bind:value={unless} data-testid="pack-unless-{r.id}" />
				{/snippet}
			</Field>
			<div class="flex flex-wrap gap-2">
				<Button type="submit" dense data-testid="pack-save-{r.id}"
					>{m.filters_packs_override_save()}</Button
				>
				{#if ov}
					<Button
						dense
						onclick={() =>
							ask(removeOverride(r.id), {
								title: m.filters_packs_remove_title({ id: r.id }),
								applyLabel: m.filters_packs_remove_apply({ id: r.id })
							})}
						data-testid="pack-remove-{r.id}">{m.filters_packs_override_remove()}</Button
					>
				{/if}
			</div>
		</form>
	</details>
</li>
