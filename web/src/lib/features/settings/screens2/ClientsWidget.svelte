<script lang="ts">
	// `integrations.clients` (§10.10.8): the download clients as cards, standing in for the JSON
	// fallback. Add, remove and reorder stage the list like any edit; Save sends it. A password is
	// its own request (never staged, so never in a stored draft), as is removing one (§10.10.7).
	import { ApiError } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import { ask } from '../form/dialogs';
	import { getSection, patchBatch } from '../form/requests';
	import type { WidgetProps } from '../form/widgets';
	import ClientCard from './ClientCard.svelte';
	import { blankClient, MAX_CLIENTS, move, toClients, type Client } from './clients';

	let { form, key, controlProps }: WidgetProps = $props();

	const f = $derived(form.field(key));
	const list = $derived(toClients(f.value));
	const saved = $derived(toClients(f.saved));
	// `<id>.password` for each client with one, as the server reports it.
	let secrets = $state<string[]>([]);
	let latest = 0;
	$effect(() => {
		void f.saved; // a save, an event or a resync changed what's saved: ask again
		const mine = ++latest;
		void getSection('integrations').then(
			(s) => {
				if (mine === latest) secrets = s.fields.clients?.secrets_set ?? [];
			},
			() => {}
		);
	});

	const set = (next: Client[]) => form.set(key, next);
	const patch = (i: number, change: Partial<Client>) =>
		set(list.map((c, j) => (j === i ? { ...c, ...change } : c)));
	// The server has a client with this id: a test can use its saved password and a password can be
	// saved for it (the server only keeps one for a saved client).
	const isSaved = (c: Client) => saved.some((x) => x.id === c.id);

	/** The saved list with one password changed, in a request of its own. Null on success. */
	async function password(id: string, value: string): Promise<string | null> {
		try {
			const current = await getSection('integrations');
			const clients = toClients(current.fields.clients?.value);
			if (!clients.some((c) => c.id === id)) return m.settings_screen_client_password_unsaved();
			await patchBatch({
				integrations: {
					if_revision: current.revision,
					clients: clients.map((c) => (c.id === id ? { ...c, password: value } : c))
				}
			});
			await form.resync();
			return null;
		} catch (e) {
			return e instanceof ApiError && e.message ? e.message : m.settings_screen_request_failed();
		}
	}

	async function removePassword(c: Client): Promise<string | null> {
		const answer = await ask({
			title: m.settings_form_secret_remove_title({ label: c.name || c.id }),
			body: [m.settings_form_secret_remove_body()],
			actions: [
				{ id: 'cancel', label: m.settings_form_cancel() },
				{ id: 'remove', label: m.settings_form_secret_remove(), variant: 'danger' }
			],
			initial: 'cancel'
		});
		return answer === 'remove' ? password(c.id, '') : null;
	}
</script>

<div
	role="group"
	aria-labelledby={controlProps['aria-labelledby']}
	aria-describedby={controlProps['aria-describedby']}
	class="flex flex-col gap-4"
>
	{#if list.length === 0}
		<p class="text-muted-text">{m.settings_screen_clients_empty()}</p>
	{/if}
	{#each list as client, i (client.id)}
		<ClientCard
			{client}
			index={i}
			count={list.length}
			locked={f.locked}
			saved={isSaved(client)}
			secretSet={secrets.includes(`${client.id}.password`)}
			onchange={(change) => patch(i, change)}
			onremove={() => set(list.filter((_, j) => j !== i))}
			onmove={(by) => set(move(list, i, i + by))}
			onpassword={(value) => password(client.id, value)}
			onremovepassword={() => removePassword(client)}
		/>
	{/each}
	{#if !f.locked}
		<div>
			<Button
				disabled={list.length >= MAX_CLIENTS}
				onclick={() => set([...list, blankClient(list)])}>{m.settings_screen_client_add()}</Button
			>
			{#if list.length >= MAX_CLIENTS}
				<span class="text-small text-muted-text"
					>{m.settings_screen_clients_max({ count: MAX_CLIENTS })}</span
				>
			{/if}
		</div>
	{/if}
</div>
