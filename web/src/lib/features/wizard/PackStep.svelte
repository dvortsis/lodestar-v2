<script lang="ts">
	// The rule-pack step (§20.6, owner §2.39, docs/decisions/2026-11-05-RP-DEF.md): rule packs are on
	// by default and the user opts out. Each box shows what the server has now: on a fresh data
	// directory every pack is on, so all five are checked. Continue sends only what the user changed:
	// - a pack checked that is off (an existing install) is turned on through the change flow's
	//   preview → apply, which records its current rule versions;
	// - a pack unchecked that is on is turned off through the weakening flow, in the Filters screen's
	//   own change dialog: the server's effect warning, then the password set in /setup.
	// Skip, or Continue with nothing changed, sends nothing, so packs stay as they are. Turning off
	// single rules is in Settings → Filters & Safety.
	import { onDestroy, onMount, type Component } from 'svelte';
	import { api } from '$lib/api/client';
	import type { components } from '$lib/api/schema';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import type { ChangeFlow } from '../filters/flows/flow.svelte';
	import { applyPacks } from './packs.svelte';

	type Pack = components['schemas']['PackState'];

	let packs = $state<Pack[]>();
	let failed = $state(false);
	let checked = $state<Record<string, boolean>>({});
	let problem = $state<{ id: string; message: string | null; on: boolean }>();
	const stop = new AbortController();
	let gen = 0;

	/** The weakening flow open for one pack: the dialog and its flow (raw: the flow is not ours). */
	let off = $state.raw<{
		id: string;
		flow: ChangeFlow;
		Dialog: Component<Record<string, unknown>>;
	}>();
	/** Whether the open flow applied, and how to tell `turnOff` it ended. */
	let offApplied = false;
	let offDone: ((applied: boolean) => void) | undefined;
	function endOff() {
		const done = offDone;
		offDone = undefined;
		off = undefined;
		done?.(offApplied);
	}

	// An existing install can have packs off: say they're offered here.
	const offered = $derived(packs?.some((p) => !p.on) ?? false);

	/** `keep`: after a change, a pack the user changed keeps their choice (it isn't lost). */
	async function load(keep = false) {
		const mine = ++gen;
		failed = false;
		try {
			const overview = await api.get('/filters', { signal: stop.signal });
			if (mine !== gen) return;
			const before = checked;
			packs = overview.packs;
			checked = Object.fromEntries(
				overview.packs.map((p) => [p.id, keep && p.id in before ? before[p.id] === true : p.on])
			);
		} catch {
			if (mine === gen && !stop.signal.aborted) failed = true;
		}
	}
	onMount(() => void load());
	onDestroy(() => {
		stop.abort();
		offApplied = false;
		endOff();
	});

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
	const name = (id: string) => names[id]?.() ?? id;

	/** Turns one pack off through the change dialog; resolves with whether it was applied. */
	function turnOff(
		id: string,
		flows: Awaited<ReturnType<typeof import('../filters/flows-door').load>>
	) {
		return new Promise<boolean>((resolve) => {
			const flow = flows.createChangeFlow();
			offApplied = false;
			offDone = resolve;
			off = { id, flow, Dialog: flows.ChangeDialog as never };
			flow.start({ op: 'pack_switch', id, on: false });
		});
	}

	/** True when the shell may move on: nothing changed, or every change went through. */
	export async function save(): Promise<boolean> {
		// Nothing was shown, so nothing is done: stay and let Retry load it.
		if (!packs) return false;
		const turnOn = packs.filter((p) => !p.on && checked[p.id]).map((p) => p.id);
		const turnOffIds = packs.filter((p) => p.on && !checked[p.id]).map((p) => p.id);
		problem = undefined;
		if (!turnOn.length && !turnOffIds.length) return true;
		// The change flow loads through the flows' door (X-flows-door-2) on the first Continue, with
		// the Filters screen's dialogs: one root keeps the flows in one on-demand chunk.
		const flows = await (await import('../filters/flows-door')).load();
		if (turnOn.length) {
			const result = await applyPacks(turnOn, flows, stop.signal);
			if (result.failed) {
				problem = { ...result.failed, on: true };
				// Whatever did go through, the screen shows the server's state; the user's other
				// choices are kept, so pressing Continue again tries them again.
				await load(true);
				return false;
			}
		}
		for (const id of turnOffIds) {
			if (stop.signal.aborted) return false;
			if (!(await turnOff(id, flows))) {
				problem = { id, message: null, on: false };
				// Cancelled or refused: the pack stays on, and the screen shows it on.
				await load();
				return false;
			}
		}
		await load(true);
		return true;
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
	{#if offered}
		<p class="text-body" data-testid="wizard-packs-offered">{m.wizard_packs_offered()}</p>
	{/if}
	<fieldset class="flex flex-col gap-3" data-testid="wizard-packs">
		<legend class="mb-1 text-small font-semibold">{m.wizard_packs_legend()}</legend>
		{#each packs as pack (pack.id)}
			<div class="flex flex-col">
				<Checkbox
					label={name(pack.id)}
					bind:checked={checked[pack.id]}
					aria-describedby="wizard-pack-{pack.id}"
					data-testid="wizard-pack-{pack.id}"
				/>
				<span id="wizard-pack-{pack.id}" class="ps-4 text-body text-muted-text"
					>{lines[pack.id]?.() ?? ''}{pack.on && !checked[pack.id]
						? ` ${m.wizard_packs_off_note()}`
						: ''}</span
				>
			</div>
		{/each}
	</fieldset>
	<p class="text-muted-text">{m.wizard_packs_rules()}</p>
	{#if problem}
		<Banner
			kind="error"
			title={problem.on
				? m.wizard_packs_failed({ pack: name(problem.id) })
				: m.wizard_packs_off_failed({ pack: name(problem.id) })}
		>
			<span data-testid="wizard-packs-error"
				>{problem.message ??
					(problem.on ? m.wizard_packs_needs_settings() : m.wizard_packs_still_on())}</span
			>
		</Banner>
	{/if}
{/if}

{#if off}
	{@const o = off}
	<o.Dialog
		flow={o.flow}
		title={m.wizard_packs_off_title({ pack: name(o.id) })}
		applyLabel={m.wizard_packs_off_apply({ pack: name(o.id) })}
		onapplied={() => (offApplied = true)}
		onclose={endOff}
	/>
{/if}
