<script lang="ts">
	// Step 8: Off / Check and notify / Install automatically (§10.10.10, §12.2). Nothing is
	// preselected beyond the server's current value (Off by default), because checking contacts
	// GitHub. A value set by the environment is shown locked.
	import { onDestroy, untrack } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import IconLock from '~icons/ph/lock-simple';
	import Radio from '$lib/ui/fields/Radio.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { createSettingsForm } from '../settings/form';
	import SettingsForm from '../settings/form/SettingsForm.svelte';

	const KEY = 'updates.mode';
	const form = untrack(() =>
		createSettingsForm({ id: 'wizard.updates', fields: [KEY], drafts: false })
	);
	onDestroy(() => form.destroy());

	export async function save(): Promise<boolean> {
		const result = await form.save();
		return result.ok || result.reason === 'nothing';
	}

	const f = $derived(form.field(KEY));
	const value = $derived(typeof f.value === 'string' ? f.value : '');
	// "Container image updates" is the server's own choice for Docker and Unraid; it is shown only
	// when it is the current value, never offered here.
	const options = $derived([
		{ value: 'off', label: m.wizard_updates_off(), help: m.wizard_updates_off_help() },
		{ value: 'notify', label: m.wizard_updates_notify(), help: m.wizard_updates_notify_help() },
		{ value: 'auto', label: m.wizard_updates_auto(), help: m.wizard_updates_auto_help() },
		...(value === 'image' || f.saved === 'image'
			? [{ value: 'image', label: m.wizard_updates_image(), help: m.wizard_updates_image_help() }]
			: [])
	]);
	const error = $derived(f.error);
</script>

<p class="text-body">{m.wizard_updates_lead()}</p>
<SettingsForm {form} saveBar={false} leaveGuard={false} shortcut={false}>
	<fieldset
		class="flex flex-col gap-3"
		aria-describedby="wizard-updates-github{error ? ' wizard-updates-error' : ''}"
		data-field={KEY}
		disabled={f.locked}
	>
		<legend class="mb-1 text-label font-semibold">{m.wizard_updates_legend()}</legend>
		{#each options as option (option.value)}
			<div class="flex flex-col">
				<Radio
					name="wizard-updates"
					label={option.label}
					value={option.value}
					group={value}
					aria-describedby="wizard-updates-{option.value}"
					onchange={() => form.set(KEY, option.value)}
				/>
				<span id="wizard-updates-{option.value}" class="ps-4 text-body text-muted-text"
					>{option.help}</span
				>
			</div>
		{/each}
	</fieldset>
	<p
		id="wizard-updates-github"
		class="text-body text-muted-text"
		data-testid="wizard-updates-github"
	>
		{m.wizard_updates_github()}
	</p>
	{#if f.locked}
		<Banner kind="info">
			<span class="flex items-center gap-2" data-testid="wizard-updates-locked"
				><IconLock aria-hidden="true" />{f.env
					? m.wizard_updates_locked({ env: f.env })
					: m.wizard_updates_locked_plain()}</span
			>
		</Banner>
	{/if}
	{#if error}<p id="wizard-updates-error" class="text-error" role="alert">{error}</p>{/if}
</SettingsForm>
