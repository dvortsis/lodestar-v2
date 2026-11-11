<script lang="ts">
	// A companion's stored credentials: names only until the admin password is entered again, then
	// one reveal that stays until Hide, never again without a new password. Pasted secrets go in a
	// password field, are sent once and are never echoed back, logged or put in an error.
	import { onDestroy } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import { ask } from '../form/dialogs';
	import { failureWords } from './logic';
	import type { CompanionsStore } from './store.svelte';

	interface Props {
		store: CompanionsStore;
		id: string;
		fields: string[];
		/** Offer the temporary-password paste (a companion Lodestar didn't install). */
		pasteOffered: boolean;
	}
	let { store, id, fields, pasteOffered }: Props = $props();

	let asking = $state<string | null>(null);
	let password = $state('');
	let revealed = $state<{ field: string; value: string } | null>(null);
	let pasted = $state('');
	let busy = $state(false);
	let error = $state('');
	let note = $state('');
	// The reveal is dropped when this panel goes away.
	onDestroy(() => (revealed = null));

	const label = (f: string) =>
		f === 'webui_password'
			? m.settings_companions_cred_webui_password()
			: f === 'api_key'
				? m.settings_companions_cred_api_key()
				: m.settings_companions_cred_other();

	async function reveal(field: string, e: Event) {
		e.preventDefault();
		busy = true;
		error = '';
		const typed = password;
		password = '';
		try {
			revealed = { field, value: await store.reveal(id, field, typed) };
			asking = null;
		} catch (x) {
			error = failureWords(x);
		} finally {
			busy = false;
		}
	}
	async function regenerate(field: string) {
		const a = await ask(
			{
				title: m.settings_companions_regen_title({ name: label(field) }),
				body: [m.settings_companions_regen_body()],
				actions: [
					{ id: 'cancel', label: m.settings_companions_cancel() },
					{ id: 'go', label: m.settings_companions_regen_action(), variant: 'danger' }
				],
				initial: 'cancel'
			},
			store.signal
		);
		if (a !== 'go') return;
		error = '';
		note = '';
		revealed = null;
		try {
			await store.regenerate(id, field);
			note = m.settings_companions_regen_done();
		} catch (x) {
			error = failureWords(x);
		}
	}
	async function paste(e: Event) {
		e.preventDefault();
		busy = true;
		error = '';
		note = '';
		const v = pasted;
		pasted = '';
		try {
			await store.paste(id, v);
			note = m.settings_companions_paste_done();
		} catch (x) {
			error = failureWords(x);
		} finally {
			busy = false;
		}
	}
</script>

<div class="flex flex-col gap-3" data-credentials={id}>
	{#each fields as f (f)}
		<div class="flex flex-col gap-2">
			<div class="flex flex-wrap items-center gap-2">
				<span class="text-small font-medium">{label(f)}</span>
				{#if revealed?.field === f}
					<Button dense onclick={() => (revealed = null)}
						>{m.settings_companions_cred_hide()}</Button
					>
				{:else}
					<Button dense onclick={() => ((asking = asking === f ? null : f), (error = ''))}>
						{m.settings_companions_cred_show()}
					</Button>
				{/if}
				<Button dense variant="danger" onclick={() => regenerate(f)}>
					{m.settings_companions_cred_regen()}
				</Button>
			</div>
			{#if revealed?.field === f}
				<p class="text-small text-muted-text">{m.settings_companions_cred_once()}</p>
				<code
					class="break-all rounded-selector border border-field-border px-2 py-1 font-mono text-small"
					data-revealed>{revealed.value}</code
				>
			{:else if asking === f}
				<form class="flex flex-col gap-2" onsubmit={(e) => reveal(f, e)}>
					<Field
						label={m.settings_companions_cred_password()}
						help={m.settings_companions_cred_password_help()}
					>
						{#snippet children(props)}
							<Input
								{...props}
								bind:value={password}
								type="password"
								autocomplete="current-password"
							/>
						{/snippet}
					</Field>
					<div>
						<Button type="submit" variant="primary" loading={busy}
							>{m.settings_companions_cred_reveal()}</Button
						>
					</div>
				</form>
			{/if}
		</div>
	{/each}
	{#if pasteOffered}
		<form class="flex flex-col gap-2" onsubmit={paste}>
			<Field label={m.settings_companions_paste_label()} help={m.settings_companions_paste_help()}>
				{#snippet children(props)}
					<Input
						{...props}
						bind:value={pasted}
						type="password"
						autocomplete="off"
						spellcheck="false"
					/>
				{/snippet}
			</Field>
			<div>
				<Button type="submit" loading={busy} aria-disabled={!pasted || undefined}
					>{m.settings_companions_paste_save()}</Button
				>
			</div>
		</form>
	{/if}
	{#if error}<Banner kind="error">{error}</Banner>{/if}
	{#if note}<Banner kind="success">{note}</Banner>{/if}
</div>
