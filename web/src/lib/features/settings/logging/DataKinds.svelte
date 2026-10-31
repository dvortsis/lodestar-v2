<script lang="ts">
	// Data in log lines (§10.10.12, §12.1): the data preset, one switch per kind (saying which of
	// your events would carry it), the two opt-ins and what is never logged. Every switch shows
	// what the server has on (`GET /logs/catalog`) and changes only through `privacy.ts`, which asks
	// first for anything identifying and sends the change at once. A click never flips a switch by
	// itself: it shows on only once the server has saved it.
	import { onDestroy, tick } from 'svelte';
	import { formatCount } from '$lib/format';
	import { m } from '$lib/paraglide/messages.js';
	import SegmentedControl from '$lib/ui/controls/SegmentedControl.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import type { SettingsForm } from '../form';
	import type { LoggingCatalog } from './catalog.svelte';
	import { eventLabel } from './event-labels';
	import { kindLabel, timeLeft } from './labels';
	import { ahead, carriers, type LogKind, type OptIn } from './rules';

	let { catalog, form }: { catalog: LoggingCatalog; form?: SettingsForm } = $props();

	const c = $derived(catalog.catalog);
	const kinds = $derived((c?.kinds ?? []).filter((k) => !k.opt_in));
	const isOn = (k: LogKind) => ahead(k.until, catalog.now);
	const PRESETS = ['private', 'hashes', 'full', 'custom'] as const;
	const PRESET_LABELS: Record<string, () => string> = {
		private: m.settings_logging_preset_private,
		hashes: m.settings_logging_preset_hashes,
		full: m.settings_logging_preset_full,
		custom: m.settings_logging_preset_custom
	};
	// The control shows only what the server has: it's redrawn after every choice, so a cancelled,
	// failed or pending one never shows as chosen once the dialog is gone.
	let presetDraw = $state(0);
	let presetGroup = $state<HTMLElement>();
	const leaving = new AbortController();
	onDestroy(() => leaving.abort());

	let busy = $state(false);
	let error = $state('');
	async function run(change: (p: typeof import('./privacy')) => Promise<unknown>, on: boolean) {
		if (busy) return;
		busy = true;
		error = '';
		try {
			await change(await import('./privacy'));
		} catch (e) {
			const message = (e as Error).message;
			error = on
				? m.settings_logging_failed_on({ message })
				: m.settings_logging_failed_off({ message });
		} finally {
			busy = false;
			const redraw = presetGroup?.contains(document.activeElement) ?? false;
			presetDraw++;
			if (redraw) {
				await tick();
				presetGroup?.querySelector<HTMLElement>('[aria-checked="true"]')?.focus();
			}
		}
	}
	const ctx = () => ({ catalog, form, signal: leaving.signal });

	// What follows on the screen (the outputs, the log files) loads on its own, after this.
	let Tail = $state.raw<typeof import('./Tail.svelte').default>();
	import('./Tail.svelte').then(
		(mod) => (Tail = mod.default),
		() => (error = m.settings_form_chunk_failed())
	);
	const choosePreset = (p: string) => run((x) => x.setPreset(ctx(), p), p !== 'private');
	const setKind = (k: LogKind, on: boolean) => run((x) => x.setKind(ctx(), k, on), on);
	const setOptIn = (k: OptIn, on: boolean) => run((x) => x.setOptIn(ctx(), k, on), on);

	function carriedBy(k: LogKind): string {
		if (!c) return '';
		const events = carriers(c, k.kind);
		if (!events.length) return m.settings_logging_kind_carried_none();
		const names = events.slice(0, 3).map(eventLabel).join(', ');
		return m.settings_logging_kind_carried({
			count: formatCount(events.length),
			events: events.length > 3 ? `${names}, …` : names
		});
	}
	const optIn = (kind: OptIn) => c?.kinds.find((k) => k.kind === kind);
</script>

<section class="flex flex-col gap-6" aria-labelledby="logging-data">
	<div>
		<h2 id="logging-data" class="text-body-lg font-semibold">{m.settings_logging_data()}</h2>
		<p class="text-muted-text">{m.settings_logging_data_help()}</p>
	</div>
	{#if error}<p role="alert" class="text-small text-error">{error}</p>{/if}
	{#if !c}
		{#if catalog.error}
			<p role="alert" class="text-small text-error">{m.settings_logging_unavailable()}</p>
		{:else}
			<p aria-busy="true" role="status">{m.settings_form_loading()}</p>
		{/if}
	{:else}
		<div bind:this={presetGroup} class="self-start">
			{#key presetDraw}
				<SegmentedControl
					label={m.settings_logging_preset()}
					options={PRESETS.map((p) => ({
						value: p,
						label: PRESET_LABELS[p]!(),
						disabled: p === 'custom'
					}))}
					value={c.presets.data}
					onchange={choosePreset}
				/>
			{/key}
		</div>
		<ul class="flex flex-col gap-4">
			{#each kinds as k (k.kind)}
				{@const on = isOn(k)}
				<li class="flex flex-col gap-1" data-testid="log-kind-{k.kind}">
					<Toggle
						label={kindLabel(k.kind)}
						checked={on}
						aria-busy={busy}
						onclick={(e) => {
							e.preventDefault();
							void setKind(k, !on);
						}}
					/>
					<p class="text-small text-muted-text">{carriedBy(k)}</p>
					{#if on && k.identifying}
						<p class="text-small" data-testid="log-kind-left-{k.kind}">
							{timeLeft(k.until, catalog.now)}
						</p>
					{/if}
				</li>
			{/each}
		</ul>

		<section class="flex flex-col gap-4" aria-labelledby="logging-optins">
			<div>
				<h3 id="logging-optins" class="text-body font-semibold">
					{m.settings_logging_optins()}
				</h3>
				<p class="text-small text-muted-text">{m.settings_logging_optins_help()}</p>
			</div>
			{#each [['peer_activity', m.settings_logging_optin_peer(), m.settings_logging_optin_peer_help()], ['filter_changes', m.settings_logging_optin_filters(), m.settings_logging_optin_filters_help()]] as const as [kind, label, help] (kind)}
				{@const k = optIn(kind)}
				{@const on = !!k && isOn(k)}
				<div class="flex flex-col gap-1" data-testid="log-optin-{kind}">
					<Toggle
						{label}
						checked={on}
						disabled={!k}
						aria-busy={busy}
						onclick={(e) => {
							e.preventDefault();
							void setOptIn(kind, !on);
						}}
					/>
					<p class="text-small text-muted-text">{help}</p>
					{#if on}<p class="text-small" data-testid="log-optin-left-{kind}">
							{timeLeft(k!.until, catalog.now)}
						</p>{/if}
				</div>
			{/each}
		</section>
	{/if}

	<section class="flex flex-col gap-3" aria-labelledby="logging-never">
		<h3 id="logging-never" class="text-body font-semibold">{m.settings_logging_never()}</h3>
		<ul class="flex flex-col gap-3">
			<li>
				<p>{m.settings_logging_never_secrets()}</p>
				<p class="text-small text-muted-text">{m.settings_logging_never_secrets_why()}</p>
			</li>
			<li>
				<p>{m.settings_logging_never_sealed()}</p>
				<p class="text-small text-muted-text">{m.settings_logging_never_sealed_why()}</p>
			</li>
		</ul>
	</section>
</section>

{#if Tail}<Tail {catalog} {form} />{/if}
