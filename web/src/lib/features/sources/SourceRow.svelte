<script lang="ts">
	// One source (§18.8): name, status, risk and consent class, enable toggle, priority and the 24 h
	// yield. Turning on a source that needs consent, a password or (Account) credentials opens its
	// dialog, which sends the enable; the server's gate (§18.7) stays the only judge. While a change is in flight the controls stay focusable (a
	// disabled control would drop the keyboard user's place) and simply ignore input.
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Select from '$lib/ui/fields/Select.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import StatusPill from './StatusPill.svelte';
	import YieldBar from './YieldBar.svelte';
	import {
		consentText,
		PRIORITIES,
		priorityText,
		riskText,
		setupNeed,
		setupText,
		statusOf,
		type Priority,
		type Source
	} from './sources';

	interface Props {
		source: Source;
		busy: boolean;
		/** Resolves to whether the server took the change. */
		onenable: (enabled: boolean) => Promise<boolean>;
		onpriority: (priority: Priority) => Promise<boolean>;
		/** Opens the consent dialog for this source's class. */
		onsetup: () => void;
		onpurge: () => void;
		/** Opens the generated settings form. */
		onsettings: () => void;
	}
	let { source: s, busy, onenable, onpriority, onsetup, onpurge, onsettings }: Props = $props();

	const id = $derived(`source-${s.id}`);
	const status = $derived(statusOf(s));
	const need = $derived(setupNeed(s));
	// The switch shows the server's state: a change it didn't take is put back by assigning this.
	let on = $derived(s.enabled);
	const locale = () => currentLocale();
	const options = $derived(PRIORITIES.map((p) => ({ value: p, label: priorityText(p) })));
	const canPrioritise = $derived(s.editable.includes('priority'));
	const canEnable = $derived(
		s.editable.includes('enabled') && s.state !== 'purged' && !s.purge_pending
	);
	const found = $derived(formatCount(s.health.harvested_24h, locale()));
	const stored = $derived(formatCount(s.health.stored_24h, locale()));
	const consent = $derived(consentText(s.consent));
	// What an acknowledgement covers, exactly as the server compares it: full addresses, so a
	// per-user path (`/~me/`) is seen before it's accepted (SRC-13c).
	const showAddresses = $derived(need === 'endpoints' || need === 'consent');

	async function toggled(e: Event & { currentTarget: HTMLInputElement }) {
		const el = e.currentTarget;
		const want = el.checked;
		// A source that needs setup opens its dialog; a change already in flight is ignored.
		if (busy || (want && (need !== null || s.consent === 'account'))) {
			el.checked = on = !want;
			if (!busy) onsetup();
			return;
		}
		if (!(await onenable(want))) el.checked = on = !want;
	}

	async function prioritised(e: Event & { currentTarget: HTMLSelectElement }) {
		const el = e.currentTarget;
		const was = s.priority;
		if (busy) {
			el.value = was;
			return;
		}
		if (!(await onpriority(el.value as Priority))) el.value = was;
	}
</script>

<li class="flex flex-col gap-2 rounded-box border border-field-border p-4" data-testid={id}>
	<div class="flex flex-wrap items-center gap-x-4 gap-y-2">
		<h3 class="text-body font-semibold">{s.name}</h3>
		<StatusPill {status} testid="{id}-status" />
		{#if s.restart_required}
			<Badge variant="info"><span data-testid="{id}-restart">{m.sources_restart()}</span></Badge>
		{/if}
		<Badge variant={s.risk === 'high' ? 'warning' : 'neutral'}
			><span data-testid="{id}-risk">{riskText(s.risk)}</span></Badge
		>
		{#if consent}<Badge><span data-testid="{id}-consent">{consent}</span></Badge>{/if}
		<div class="ms-auto flex flex-wrap items-center gap-x-4 gap-y-2">
			<Toggle
				label={m.sources_enable()}
				aria-label={m.sources_toggle_name({ name: s.name })}
				bind:checked={on}
				disabled={!canEnable}
				aria-busy={busy}
				aria-describedby={need !== null ? `${id}-need` : undefined}
				onchange={toggled}
				data-testid="{id}-toggle"
			/>
			<label class="flex items-center gap-2 text-small">
				<span aria-hidden="true">{m.sources_priority()}</span>
				<Select
					dense
					class="w-auto"
					{options}
					value={s.priority}
					disabled={!canPrioritise}
					aria-label={m.sources_priority_name({ name: s.name })}
					aria-busy={busy}
					onchange={prioritised}
					data-testid="{id}-priority"
				/>
			</label>
		</div>
	</div>
	{#if need !== null}
		<p class="text-small" id="{id}-need" data-testid="{id}-need">
			{setupText(need)}
		</p>
	{/if}
	{#if showAddresses}
		<div class="flex flex-col gap-1 text-small" data-testid="{id}-addresses">
			<p>{m.sources_addresses_heading()}</p>
			{#if s.endpoints.length > 0}
				<ul class="flex flex-col gap-1">
					{#each s.endpoints as address (address)}
						<li><code class="font-mono break-all">{address}</code></li>
					{/each}
				</ul>
			{:else}
				<p class="text-muted-text">{m.sources_addresses_none()}</p>
			{/if}
		</div>
	{/if}
	{#if s.health.last_error}
		<p class="text-small text-error" data-testid="{id}-last-error">
			{m.sources_last_error({ error: s.health.last_error })}
		</p>
	{/if}
	<div class="flex flex-wrap items-center gap-2 text-small text-muted-text">
		<YieldBar found={s.health.harvested_24h} stored={s.health.stored_24h} testid="{id}-yield" />
		<span data-testid="{id}-found">{m.sources_found({ count: found })}</span>
		<span aria-hidden="true">·</span>
		<span data-testid="{id}-stored">{m.sources_stored({ count: stored })}</span>
		{#if s.editable.includes('config') && s.state !== 'purged'}
			<Button dense class="ms-auto" onclick={onsettings} data-testid="{id}-settings">
				{m.sources_settings()}<span class="sr-only"> {s.name}</span>
			</Button>
		{/if}
		{#if s.state !== 'purged'}
			<Button
				dense
				class={s.editable.includes('config') ? '' : 'ms-auto'}
				onclick={onpurge}
				data-testid="{id}-purge"
			>
				{m.sources_purge_action()}<span class="sr-only"> {s.name}</span>
			</Button>
		{/if}
	</div>
</li>
