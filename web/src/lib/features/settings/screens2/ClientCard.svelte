<script lang="ts">
	// One download client (§10.10.8): its fields, **Test connection** on the values on screen, and
	// its password. The password never joins the form's staged value, so it can't reach a stored
	// draft or Undo: it is typed here, tested from here, and saved or removed by its own request
	// (the parent's `onPassword`), after the client itself is saved.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Select from '$lib/ui/fields/Select.svelte';
	import IconSet from '~icons/ph/check-circle';
	import IconUnset from '~icons/ph/circle-dashed';
	import { KINDS, type Client } from './clients';
	import TestButton from './TestButton.svelte';

	interface Props {
		client: Client;
		index: number;
		count: number;
		locked: boolean;
		/** The server has this client: its saved password can be used, replaced or removed. */
		saved: boolean;
		secretSet: boolean;
		onchange: (patch: Partial<Client>) => void;
		onremove: () => void;
		onmove: (by: -1 | 1) => void;
		onpassword: (value: string) => Promise<string | null>;
		onremovepassword: () => Promise<string | null>;
	}
	let {
		client,
		index,
		count,
		locked,
		saved,
		secretSet,
		onchange,
		onremove,
		onmove,
		onpassword,
		onremovepassword
	}: Props = $props();

	const name = $derived(client.name || m.settings_screen_client_unnamed({ number: index + 1 }));
	let password = $state('');
	let busy = $state(false);
	let problem = $state('');

	async function run(job: () => Promise<string | null>, done?: () => void) {
		busy = true;
		problem = '';
		const error = await job();
		busy = false;
		if (error) problem = error;
		else done?.();
	}

	// Nothing the test needs is sent twice: a typed password is sent as it is; without one, the
	// saved password is used by the server, but only for the saved address and kind.
	const build = () =>
		client.url.trim() === ''
			? null
			: ({
					kind: 'download_client',
					client: client.kind as 'qbittorrent',
					url: client.url.trim(),
					...(client.username ? { username: client.username } : {}),
					...(saved ? { id: client.id } : {}),
					...(password ? { password } : {})
				} as const);
</script>

<div
	role="group"
	aria-label={name}
	class="flex flex-col gap-4 rounded-lg border border-base-300 p-4"
	data-testid="client-card"
>
	<div class="flex flex-wrap items-center justify-between gap-2">
		<h3 class="text-label font-semibold">{name}</h3>
		{#if !locked}
			<div class="flex flex-wrap gap-1">
				<Button
					dense
					variant="quiet"
					disabled={index === 0}
					onclick={() => onmove(-1)}
					aria-label={m.settings_screen_client_up({ name })}
					>{m.settings_screen_client_move_up()}</Button
				>
				<Button
					dense
					variant="quiet"
					disabled={index === count - 1}
					onclick={() => onmove(1)}
					aria-label={m.settings_screen_client_down({ name })}
					>{m.settings_screen_client_move_down()}</Button
				>
				<Button
					dense
					variant="danger"
					onclick={onremove}
					aria-label={m.settings_screen_client_remove_named({ name })}
					>{m.settings_screen_client_remove()}</Button
				>
			</div>
		{/if}
	</div>
	<div class="grid gap-4 sm:grid-cols-2">
		<Field label={m.settings_screen_client_name()}>
			{#snippet children(cp)}
				<Input
					{...cp}
					value={client.name}
					disabled={locked}
					oninput={(e) => onchange({ name: e.currentTarget.value })}
				/>
			{/snippet}
		</Field>
		<Field label={m.settings_screen_client_kind()}>
			{#snippet children(cp)}
				<Select
					{...cp}
					options={[...KINDS]}
					value={client.kind}
					disabled={locked}
					onchange={(e) => onchange({ kind: e.currentTarget.value })}
				/>
			{/snippet}
		</Field>
		<Field label={m.settings_screen_client_url()} help={m.settings_screen_client_url_help()}>
			{#snippet children(cp)}
				<Input
					{...cp}
					mono
					type="url"
					autocomplete="off"
					spellcheck={false}
					value={client.url}
					disabled={locked}
					oninput={(e) => onchange({ url: e.currentTarget.value })}
				/>
			{/snippet}
		</Field>
		<Field label={m.settings_screen_client_username()}>
			{#snippet children(cp)}
				<Input
					{...cp}
					autocomplete="off"
					value={client.username}
					disabled={locked}
					oninput={(e) => onchange({ username: e.currentTarget.value })}
				/>
			{/snippet}
		</Field>
		<Field label={m.settings_screen_client_category()}>
			{#snippet children(cp)}
				<Input
					{...cp}
					value={client.category}
					disabled={locked}
					oninput={(e) => onchange({ category: e.currentTarget.value })}
				/>
			{/snippet}
		</Field>
		<Field label={m.settings_screen_client_save_path()}>
			{#snippet children(cp)}
				<Input
					{...cp}
					mono
					spellcheck={false}
					value={client.save_path}
					disabled={locked}
					oninput={(e) => onchange({ save_path: e.currentTarget.value })}
				/>
			{/snippet}
		</Field>
	</div>
	<Checkbox
		label={m.settings_screen_client_paused()}
		checked={client.paused}
		disabled={locked}
		onchange={(e) => onchange({ paused: e.currentTarget.checked })}
	/>

	<div class="flex flex-col gap-2">
		<span class="inline-flex items-center gap-1 text-body" data-testid="client-password-status">
			{#if secretSet}
				<IconSet class="size-(--icon-size-row) shrink-0 text-success" aria-hidden="true" />
				{m.settings_form_secret_set()}
			{:else}
				<IconUnset class="size-(--icon-size-row) shrink-0 text-muted-text" aria-hidden="true" />
				{m.settings_form_secret_not_set()}
			{/if}
			<span class="text-muted-text">· {m.settings_screen_client_password()}</span>
		</span>
		<Field
			label={m.settings_screen_client_password_new()}
			help={saved
				? m.settings_screen_client_password_help()
				: m.settings_screen_client_password_unsaved()}
			error={problem || undefined}
		>
			{#snippet children(cp)}
				<Input
					{...cp}
					type="password"
					autocomplete="new-password"
					spellcheck={false}
					bind:value={password}
					disabled={locked}
				/>
			{/snippet}
		</Field>
		<div class="flex flex-wrap gap-2">
			<Button
				dense
				disabled={locked || !saved || password === '' || busy}
				onclick={() =>
					run(
						() => onpassword(password),
						() => (password = '')
					)}>{m.settings_screen_client_password_save()}</Button
			>
			{#if secretSet && !locked}
				<Button
					dense
					variant="danger"
					disabled={!saved || busy}
					onclick={() => run(onremovepassword)}>{m.settings_form_secret_remove()}</Button
				>
			{/if}
		</div>
	</div>

	<TestButton
		label={m.settings_screen_test_connection()}
		aria-label={m.settings_screen_test_connection_named({ name })}
		{build}
		missing={m.settings_screen_test_need_url()}
		success={(version) =>
			version
				? m.settings_screen_test_connected_version({ version })
				: m.settings_screen_test_connected()}
	/>
</div>
