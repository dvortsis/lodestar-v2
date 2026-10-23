<script lang="ts">
	// Tests only: a screen built the way S-settings-1 builds one.
	import SettingField from '../SettingField.svelte';
	import SettingsForm from '../SettingsForm.svelte';
	import type { SettingsForm as Form } from '../form.svelte';
	import type { FieldKey } from '../types';

	interface Props {
		form: Form;
		saveBar?: boolean;
		onReveal?: (key: FieldKey) => void;
	}

	let { form, saveBar = true, onReveal }: Props = $props();
</script>

<SettingsForm {form} {saveBar} {onReveal}>
	{#each form.specs as s (s.key)}
		<SettingField {form} key={s.key} />
	{/each}
</SettingsForm>
