<script lang="ts">
	// The rule-pack step (§20.6, owner §2.8): rule packs are off until chosen, and all five are
	// pre-checked here. Continue turns the checked ones on through the filters change flows; Skip, or
	// none checked, sends nothing, so packs stay off. If the server already has packs on, its state
	// is shown instead of pre-checking, and a pack that is on cannot be turned off here: this step
	// only ever turns packs on.
	import { onDestroy, onMount } from 'svelte';
	import { api } from '$lib/api/client';
	import type { components } from '$lib/api/schema';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { applyPacks } from './packs.svelte';

	type Pack = components['schemas']['PackState'];

	let packs = $state<Pack[]>();
	let failed = $state(false);
	let checked = $state<Record<string, boolean>>({});
	let problem = $state<{ id: string; message: string | null }>();
	const stop = new AbortController();
	let gen = 0;

	// With any pack on, the server has a stored choice: show it as it is.
	const stored = $derived(packs?.some((p) => p.on) ?? false);

	/** `keep`: after an apply, a pack still off keeps the user's check (their choice isn't lost). */
	async function load(keep = false) {
		const mine = ++gen;
		failed = false;
		try {
			const overview = await api.get('/filters', { signal: stop.signal });
			if (mine !== gen) return;
			packs = overview.packs;
			const any = overview.packs.some((p) => p.on);
			const before = checked;
			checked = Object.fromEntries(
				overview.packs.map((p) => [p.id, p.on || (keep ? before[p.id] === true : !any)])
			);
		} catch {
			if (mine === gen && !stop.signal.aborted) failed = true;
		}
	}
	onMount(() => void load());
	onDestroy(() => stop.abort());

	const names: Record<string, () => string> = {
		malware: () => m.wizard_pack_malware(),
		lures: () => m.wizard_pack_lures(),
		fakes: () => m.wizard_pack_fakes(),
		spam: () => m.wizard_pack_spam(),
		scams: () => m.wizard_pack_scams()
	};
	const lines: Record<string, () => string> = {
		malware: () => m.wizard_pack_malware_text(),
		lures: () => m.wizard_pack_lures_text(),
		fakes: () => m.wizard_pack_fakes_text(),
		spam: () => m.wizard_pack_spam_text(),
		scams: () => m.wizard_pack_scams_text()
	};

	/** True when the shell may move on: nothing to turn on, or every checked pack is now on. */
	export async function save(): Promise<boolean> {
		// Nothing was shown, so nothing is done: stay and let Retry load it.
		if (!packs) return false;
		const ids = (packs ?? []).filter((p) => !p.on && checked[p.id]).map((p) => p.id);
		problem = undefined;
		if (!ids.length) return true;
		// The change flow loads through the flows' door (X-flows-door-2) on the first Continue, with
		// the Filters screen's dialogs: one root keeps the flows in one on-demand chunk.
		const flows = await (await import('../filters/flows-door')).load();
		const result = await applyPacks(ids, flows, stop.signal);
		if (!result.failed) {
			await load(true);
			return true;
		}
		problem = result.failed;
		// Whatever did go through, the screen shows the server's state; the packs still off keep
		// their checks, so pressing Continue again tries them again.
		await load(true);
		return false;
	}
</script>

<p class="text-body">{m.wizard_packs_lead()}</p>
{#if failed}
	<Banner kind="error">
		{m.wizard_packs_load_failed()}
		{#snippet action()}
			<Button dense onclick={() => void load()}>{m.wizard_retry()}</Button>
		{/snippet}
	</Banner>
{:else if !packs}
	<p class="text-muted-text" aria-busy="true">{m.wizard_loading()}</p>
{:else}
	{#if stored}
		<p class="text-body" data-testid="wizard-packs-stored">{m.wizard_packs_stored()}</p>
	{/if}
	<fieldset class="flex flex-col gap-3" data-testid="wizard-packs">
		<legend class="mb-1 text-label font-semibold">{m.wizard_packs_legend()}</legend>
		{#each packs as pack (pack.id)}
			<div class="flex flex-col">
				<Checkbox
					label={names[pack.id]?.() ?? pack.id}
					bind:checked={checked[pack.id]}
					disabled={pack.on}
					aria-describedby="wizard-pack-{pack.id}"
					data-testid="wizard-pack-{pack.id}"
				/>
				<span id="wizard-pack-{pack.id}" class="ps-4 text-body text-muted-text"
					>{pack.on ? m.wizard_packs_on() : (lines[pack.id]?.() ?? '')}</span
				>
			</div>
		{/each}
	</fieldset>
	{#if problem}
		<Banner
			kind="error"
			title={m.wizard_packs_failed({ pack: names[problem.id]?.() ?? problem.id })}
		>
			<span data-testid="wizard-packs-error"
				>{problem.message ?? m.wizard_packs_needs_settings()}</span
			>
		</Banner>
	{/if}
{/if}
