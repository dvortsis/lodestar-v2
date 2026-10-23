<script lang="ts">
	// A secret (`writeOnly`, §10.10.4): never pre-filled, never shown. "Set" when the server holds
	// one, with Replace (an empty password input, staged into Save and cleared after every save
	// attempt) and Remove (destructive, its own PATCH after a confirmation, §10.10.7).
	import Button from '$lib/ui/controls/Button.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { ask } from './dialogs';
	import type { SettingsForm } from './form.svelte';
	import type { FieldKey } from './types';
	import type { FieldControlProps } from './widgets';

	interface Props {
		form: SettingsForm;
		key: FieldKey;
		controlProps: FieldControlProps;
		label: string;
		dense?: boolean;
	}

	let { form, key, controlProps, label, dense = false }: Props = $props();

	const f = $derived(form.field(key));
	let replacing = $state(false);
	// The input shows only what's staged: the store drops it after each save attempt.
	const staged = $derived(typeof f.value === 'string' ? f.value : '');

	async function remove() {
		const answer = await ask({
			title: m.settings_form_secret_remove_title({ label }),
			body: [m.settings_form_secret_remove_body()],
			actions: [
				{ id: 'cancel', label: m.settings_form_cancel() },
				{ id: 'remove', label: m.settings_form_secret_remove(), variant: 'danger' }
			],
			initial: 'cancel'
		});
		if (answer === 'remove') await form.removeSecret(key);
	}
</script>

<div class="flex flex-col gap-2">
	<div class="flex flex-wrap items-center gap-2">
		<span class="text-body" data-testid="secret-status">
			{f.secretSet ? m.settings_form_secret_set() : m.settings_form_secret_not_set()}
		</span>
		{#if !f.locked}
			{#if !replacing}
				<Button {dense} onclick={() => (replacing = true)}>
					{f.secretSet ? m.settings_form_secret_replace() : m.settings_form_secret_add()}
				</Button>
			{/if}
			{#if f.secretSet}
				<Button {dense} variant="danger" onclick={remove}>{m.settings_form_secret_remove()}</Button>
			{/if}
		{/if}
	</div>
	{#if replacing && !f.locked}
		<Input
			{...controlProps}
			{dense}
			type="password"
			autocomplete="new-password"
			spellcheck={false}
			value={staged}
			aria-label={m.settings_form_secret_new({ label })}
			oninput={(e) => form.set(key, e.currentTarget.value)}
		/>
	{/if}
</div>
