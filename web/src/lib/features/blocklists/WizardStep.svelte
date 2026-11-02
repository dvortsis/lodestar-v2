<script lang="ts">
	// The wizard's blocklist step (§20.13, owner §2.9): optional, off by default, nothing
	// pre-selected. Each chosen list shows how strict your lists would be (the server's own
	// assessment of the chosen set) before Continue connects them. Skip, or none chosen, connects
	// nothing. Connecting never asks for the password here: anything that would (or warns) is left
	// to Settings → Blocklist subscriptions.
	import { onDestroy, onMount } from 'svelte';
	import { api, isAbort } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { connectLists } from './connect.svelte';
	import type { Strictness, Subscriptions } from './lists';
	import StrictnessNote from './StrictnessNote.svelte';

	let data = $state.raw<Subscriptions>();
	let failed = $state(false);
	let checked = $state<Record<string, boolean>>({});
	let strictness = $state.raw<Strictness>();
	let strictFailed = $state(false);
	let problem = $state<{ name: string; message: string | null }>();
	const stop = new AbortController();
	let gen = 0;

	const chosen = $derived(
		(data?.catalog ?? []).filter((l) => l.subscription === null && checked[l.key])
	);

	async function load() {
		failed = false;
		try {
			const got = await api.get('/filters/subscriptions', { signal: stop.signal });
			// Nothing is pre-selected; a choice already made stays.
			checked = Object.fromEntries(got.catalog.map((l) => [l.key, checked[l.key] === true]));
			data = got;
		} catch {
			if (!stop.signal.aborted) failed = true;
		}
	}
	onMount(() => void load());
	onDestroy(() => stop.abort());

	// What the chosen set would be, together with the lists already connected and enabled.
	$effect(() => {
		const keys = chosen.map((l) => l.key);
		const mine = ++gen;
		strictness = undefined;
		strictFailed = false;
		if (!keys.length || !data) return;
		const ids = data.connected.filter((c) => c.subscription.enabled).map((c) => c.subscription.id);
		api
			.post('/filters/strictness', {
				body: { catalog: keys, subscriptions: ids },
				signal: stop.signal
			})
			.then((s) => {
				if (mine === gen) strictness = s;
			})
			.catch((e) => {
				if (mine === gen && !isAbort(e)) strictFailed = true;
			});
	});

	/** True when the shell may move on: nothing chosen, or every chosen list is now connected. */
	export async function save(): Promise<boolean> {
		if (!data) return false;
		problem = undefined;
		if (!chosen.length) return true;
		// The warning for this exact choice must have been shown before anything is connected.
		if (!strictness) return false;
		const flows = await (await import('../filters/flows-door')).load();
		const result = await connectLists(chosen, flows, stop.signal);
		if (!result.failed) {
			await load();
			return true;
		}
		problem = { name: result.failed.list.name, message: result.failed.message };
		await load();
		return false;
	}
</script>

<p class="text-body">{m.blocklists_wizard_lead()}</p>
{#if failed}
	<Banner kind="error">
		{m.blocklists_wizard_load_failed()}
		{#snippet action()}<Button dense onclick={() => void load()}>{m.blocklists_retry()}</Button
			>{/snippet}
	</Banner>
{:else if !data}
	<p class="text-muted-text" aria-busy="true">{m.blocklists_loading()}</p>
{:else}
	<fieldset class="flex flex-col gap-3" data-testid="wizard-blocklists">
		<legend class="mb-1 text-small font-semibold">{m.blocklists_wizard_legend()}</legend>
		{#each data.catalog as l (l.key)}
			<div class="flex flex-col">
				<Checkbox
					label={l.name}
					bind:checked={checked[l.key]}
					disabled={l.subscription !== null}
					aria-describedby="wizard-blocklist-{l.key}"
					data-testid="wizard-blocklist-{l.key}"
				/>
				<span id="wizard-blocklist-{l.key}" class="ps-4 text-body text-muted-text"
					>{l.subscription !== null ? m.blocklists_wizard_connected() : l.about}</span
				>
			</div>
		{/each}
	</fieldset>
	{#if !chosen.length}
		<p class="text-muted-text" data-testid="wizard-blocklists-none">{m.blocklists_wizard_none()}</p>
	{:else if strictness}
		<StrictnessNote {strictness} />
	{:else if strictFailed}
		<Banner kind="error">{m.blocklists_wizard_strictness_failed()}</Banner>
	{:else}
		<p class="text-muted-text" aria-busy="true">{m.blocklists_loading()}</p>
	{/if}
	{#if problem}
		<Banner kind="error">
			<span data-testid="wizard-blocklists-error"
				>{problem.message === null
					? m.blocklists_wizard_needs_settings({ name: problem.name })
					: m.blocklists_wizard_failed({ name: problem.name, message: problem.message })}</span
			>
		</Banner>
	{/if}
{/if}
