<script lang="ts">
	// A wizard step made of the Settings engine's own fields (§10.10.10): the same `SettingField`s,
	// previews and validation, saved through the same multi-section patch. Skip never reaches this
	// component's save, so it sends nothing.
	import { onDestroy, untrack } from 'svelte';
	import { createSettingsForm } from '../settings/form';
	import type { FieldKey } from '../settings/form';
	import SettingField from '../settings/form/SettingField.svelte';
	import SettingsForm from '../settings/form/SettingsForm.svelte';

	interface Props {
		/** The draft scope: `wizard.intensity`. */
		id: string;
		keys: FieldKey[];
		lead: string;
	}
	let { id, keys, lead }: Props = $props();

	const form = untrack(() => createSettingsForm({ id, fields: keys, drafts: false }));
	onDestroy(() => form.destroy());

	/** True when the shell may move on: saved, or nothing to save. A refusal stays on the step. */
	let restart = false;
	/** Whether a saved field applies only after a restart (the shell tells Done). */
	export const needsRestart = () => restart;
	export async function save(): Promise<boolean> {
		// Nothing was shown, so nothing is done: stay and let the engine's Retry load it.
		if (form.status === 'loading' || form.status === 'load-error') return false;
		const result = await form.save();
		if (result.ok && result.restart.length) restart = true;
		return result.ok || result.reason === 'nothing';
	}
</script>

<p class="text-body">{lead}</p>
<SettingsForm {form} saveBar={false} leaveGuard={false} shortcut={false}>
	{#each form.specs as spec (spec.key)}
		<SettingField {form} key={spec.key} />
	{/each}
</SettingsForm>
