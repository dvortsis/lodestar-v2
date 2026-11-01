<script lang="ts">
	// Your rules, the override list (CSX-5), exceptions and Always allow. Disabling a rule, adding
	// an exception or an Always allow entry, and every override loosen filtering: each goes through
	// the flow's dialog, and Cancel sends nothing. Adding a rule (stricter) uses the same dialog.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Select from '$lib/ui/fields/Select.svelte';
	import {
		addAllow,
		addRule,
		removeAllow,
		removeException,
		removeOverride,
		removeRule,
		setRuleEnabled
	} from './ops';
	import type { Ask } from './types';
	import type { Overview } from './model.svelte';

	let { o, ask, reload }: { o: Overview; ask: Ask; reload: () => void } = $props();

	const ACTIONS = ['flag', 'demote', 'hide', 'quarantine', 'drop'] as const;
	const label: Record<string, () => string> = {
		flag: m.filters_action_flag,
		demote: m.filters_action_demote,
		hide: m.filters_action_hide,
		quarantine: m.filters_action_quarantine,
		drop: m.filters_action_drop
	};
	const actionText = (a: string | null) =>
		a && label[a] ? label[a]!() : m.filters_action_unknown();

	let query = $state('');
	let action = $state<string>('hide');
	let allow = $state('');
	let tried = $state(false);
	let allowTried = $state(false);
	const queryError = $derived(tried && !query.trim() ? m.filters_invalid_query() : undefined);

	function add() {
		tried = true;
		if (!query.trim()) return;
		ask(addRule(query.trim(), action), {
			title: m.filters_rule_add_title(),
			shadowDays: 7
		});
	}
	const entry = (e: { hashes?: string[]; keyword?: string; payload?: string; query?: string }) =>
		e.keyword ?? e.query ?? e.payload ?? (e.hashes ?? []).join(' ');
	let OverrideDialog = $state.raw<typeof import('../flows/OverrideDialog.svelte').default>();
	let overrideOpen = $state(false);
	async function openOverride() {
		OverrideDialog ??= (await (await import('../flows-door')).load()).OverrideDialog;
		overrideOpen = true;
	}
</script>

<section class="flex flex-col gap-3" aria-labelledby="filters-rules-h">
	<h2 id="filters-rules-h" class="text-small font-semibold">{m.filters_rules_heading()}</h2>
	{#if o.rules.length === 0}
		<p class="text-muted-text">{m.filters_rules_empty()}</p>
	{/if}
	<ul class="flex flex-col gap-2" data-testid="filters-rules">
		{#each o.rules as r (r.id)}
			<li class="flex flex-wrap items-center gap-3" data-testid="rule-{r.id}">
				<Mono wrap>{r.query}</Mono>
				<Badge>{actionText(r.action)}</Badge>
				{#if r.shadow_until}<Badge variant="info">{m.filters_rule_shadow()}</Badge>{/if}
				{#if !r.enabled}<Badge variant="warning">{m.filters_state_off()}</Badge>{/if}
				<Button
					dense
					onclick={() =>
						ask(setRuleEnabled(r.id, !r.enabled), {
							title: r.enabled ? m.filters_rule_disable_title() : m.filters_rule_enable_title(),
							applyLabel: r.enabled ? m.filters_rule_disable() : m.filters_rule_enable()
						})}
					data-testid="rule-toggle-{r.id}"
					>{r.enabled ? m.filters_rule_disable() : m.filters_rule_enable()}</Button
				>
				<Button
					dense
					onclick={() =>
						ask(removeRule(r.id), {
							title: m.filters_rule_remove_title(),
							applyLabel: m.filters_rule_remove()
						})}
					data-testid="rule-remove-{r.id}">{m.filters_rule_remove()}</Button
				>
			</li>
		{/each}
	</ul>
	<form
		class="flex flex-wrap items-end gap-3"
		onsubmit={(e) => {
			e.preventDefault();
			add();
		}}
	>
		<Field label={m.filters_rule_query()} error={queryError}>
			{#snippet children(a)}
				<Input {...a} bind:value={query} name="rule-query" autocomplete="off" />
			{/snippet}
		</Field>
		<Field label={m.filters_rule_action()}>
			{#snippet children(a)}
				<Select
					{...a}
					bind:value={action}
					name="rule-action"
					options={ACTIONS.map((v) => ({ value: v, label: label[v]!() }))}
				/>
			{/snippet}
		</Field>
		<Button type="submit" data-testid="rule-add">{m.filters_rule_add()}</Button>
	</form>
</section>

<section class="flex flex-col gap-3" aria-labelledby="filters-overrides-h">
	<h2 id="filters-overrides-h" class="text-small font-semibold">{m.filters_overrides_heading()}</h2>
	<p class="text-muted-text">{m.filters_overrides_help()}</p>
	<ul class="flex flex-col gap-2" data-testid="filters-overrides">
		{#each o.overrides as v (v.id)}
			<li class="flex flex-wrap items-center gap-3" data-testid="override-{v.id}">
				<Mono wrap>{entry(v)}</Mono>
				<Badge variant={v.strength === 'override' ? 'warning' : 'neutral'}
					>{v.strength === 'override'
						? m.filters_strength_override()
						: m.filters_strength_neutralize()}</Badge
				>
				<Button
					dense
					onclick={() =>
						ask(removeOverride(v.id), {
							title: m.filters_override_remove_title(),
							applyLabel: m.filters_override_remove()
						})}
					data-testid="override-remove-{v.id}">{m.filters_override_remove()}</Button
				>
			</li>
		{/each}
	</ul>
	<div>
		<Button onclick={openOverride} data-testid="override-add">{m.filters_override_add()}</Button>
	</div>
</section>

<section class="flex flex-col gap-3" aria-labelledby="filters-exceptions-h">
	<h2 id="filters-exceptions-h" class="text-small font-semibold">
		{m.filters_exceptions_heading()}
	</h2>
	<p class="text-muted-text">{m.filters_exceptions_help()}</p>
	<ul class="flex flex-col gap-2" data-testid="filters-exceptions">
		{#each o.exceptions as x (x.id)}
			<li class="flex flex-wrap items-center gap-3" data-testid="exception-{x.id}">
				<Mono wrap>{entry(x)}</Mono>
				<Badge>{x.rule}</Badge>
				<Button
					dense
					onclick={() =>
						ask(removeException(x.id), {
							title: m.filters_exception_remove_title(),
							applyLabel: m.filters_exception_remove()
						})}
					data-testid="exception-remove-{x.id}">{m.filters_exception_remove()}</Button
				>
			</li>
		{/each}
	</ul>
</section>

<section class="flex flex-col gap-3" aria-labelledby="filters-allow-h">
	<h2 id="filters-allow-h" class="text-small font-semibold">{m.filters_allow_heading()}</h2>
	<p class="text-muted-text">{m.filters_allow_help()}</p>
	<ul class="flex flex-col gap-2" data-testid="filters-allow">
		{#each o.allow as a (a.id)}
			<li class="flex flex-wrap items-center gap-3" data-testid="allow-{a.id}">
				<Mono wrap>{a.query}</Mono>
				<Button
					dense
					onclick={() =>
						ask(removeAllow(a.id), {
							title: m.filters_allow_remove_title(),
							applyLabel: m.filters_allow_remove()
						})}
					data-testid="allow-remove-{a.id}">{m.filters_allow_remove()}</Button
				>
			</li>
		{/each}
	</ul>
	<form
		class="flex flex-wrap items-end gap-3"
		onsubmit={(e) => {
			e.preventDefault();
			allowTried = true;
			if (!allow.trim()) return;
			ask(addAllow(allow.trim()), {
				title: m.filters_allow_add_title(),
				applyLabel: m.filters_allow_add()
			});
		}}
	>
		<Field
			label={m.filters_rule_query()}
			error={allowTried && !allow.trim() ? m.filters_invalid_query() : undefined}
		>
			{#snippet children(a)}
				<Input {...a} bind:value={allow} name="allow-query" autocomplete="off" />
			{/snippet}
		</Field>
		<Button type="submit" data-testid="allow-add">{m.filters_allow_add()}</Button>
	</form>
</section>

{#if OverrideDialog}
	<OverrideDialog bind:open={overrideOpen} shadowDays={7} onapplied={reload} />
{/if}
