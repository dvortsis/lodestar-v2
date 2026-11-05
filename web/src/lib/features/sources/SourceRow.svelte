<script lang="ts">
	// One source (§18.8): name, status, enable toggle, priority and the 24 h yield. The toggle never
	// asks the server to turn on a source that still needs setup (consent, credentials, password):
	// the dialogs for those are SRC-15b, and the server's gate (§18.7) stays the only judge.
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Select from '$lib/ui/fields/Select.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import StatusPill from './StatusPill.svelte';
	import YieldBar from './YieldBar.svelte';
	import {
		PRIORITIES,
		priorityText,
		setupNeed,
		setupText,
		statusOf,
		type Priority,
		type Source
	} from './sources';

	interface Props {
		source: Source;
		busy: boolean;
		onenable: (enabled: boolean) => void;
		onpriority: (priority: Priority) => void;
	}
	let { source: s, busy, onenable, onpriority }: Props = $props();

	const id = $derived(`source-${s.id}`);
	const status = $derived(statusOf(s));
	const need = $derived(setupNeed(s));
	const locale = () => currentLocale();
	const options = $derived(PRIORITIES.map((p) => ({ value: p, label: priorityText(p) })));
	const canPrioritise = $derived(s.editable.includes('priority'));
	const canEnable = $derived(s.editable.includes('enabled') && s.state !== 'purged');
	const found = $derived(formatCount(s.health.harvested_24h, locale()));
	const stored = $derived(formatCount(s.health.stored_24h, locale()));
</script>

<li class="flex flex-col gap-2 rounded-box border border-field-border p-4" data-testid={id}>
	<div class="flex flex-wrap items-center gap-x-4 gap-y-2">
		<h3 class="text-body font-semibold">{s.name}</h3>
		<StatusPill {status} testid="{id}-status" />
		{#if s.restart_required}
			<Badge variant="info"><span data-testid="{id}-restart">{m.sources_restart()}</span></Badge>
		{/if}
		<div class="ms-auto flex flex-wrap items-center gap-x-4 gap-y-2">
			<Toggle
				label={m.sources_enable()}
				checked={s.enabled}
				disabled={busy || !canEnable || (need !== null && !s.enabled)}
				aria-describedby={need !== null ? `${id}-need` : undefined}
				onchange={(e) => {
					// Turning on is the server's gate; a source that needs setup never asks it.
					if (e.currentTarget.checked && need !== null) e.currentTarget.checked = false;
					else onenable(e.currentTarget.checked);
				}}
				data-testid="{id}-toggle"
			/>
			<label class="flex items-center gap-2 text-small">
				<span>{m.sources_priority()}</span>
				<Select
					dense
					class="w-auto"
					{options}
					value={s.priority}
					disabled={busy || !canPrioritise}
					onchange={(e) => onpriority(e.currentTarget.value as Priority)}
					data-testid="{id}-priority"
				/>
			</label>
		</div>
	</div>
	{#if need !== null}
		<p class="text-small text-warning-content" id="{id}-need" data-testid="{id}-need">
			{setupText(need)}
		</p>
	{/if}
	{#if s.health.last_error}
		<p class="text-small text-error" data-testid="{id}-last-error">
			{m.sources_last_error({ error: s.health.last_error })}
		</p>
	{/if}
	<div class="flex flex-wrap items-center gap-2 text-small text-muted-text">
		<YieldBar
			found={s.health.harvested_24h}
			stored={s.health.stored_24h}
			label={m.sources_yield_label({ found, stored })}
			testid="{id}-yield"
		/>
		<span data-testid="{id}-found">{m.sources_found({ count: found })}</span>
		<span aria-hidden="true">·</span>
		<span data-testid="{id}-stored">{m.sources_stored({ count: stored })}</span>
	</div>
</li>
