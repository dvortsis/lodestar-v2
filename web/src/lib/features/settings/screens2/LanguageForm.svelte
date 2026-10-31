<script lang="ts">
	// The Language & region form: the engine in instant mode (Preferences apply at once, with
	// Undo, §10.10.1). Saved language, time zone and first day apply here at once; the layout
	// applies them at start-up (lib/i18n/boot.ts).
	import { onDestroy, untrack } from 'svelte';
	import { followSaved, narrowRegion } from '$lib/i18n/region.svelte';
	import { createSettingsForm, type FieldSpec } from '../form';
	import SettingField from '../form/SettingField.svelte';
	import SettingsForm from '../form/SettingsForm.svelte';
	import { registerWidget } from '../form/widgets';
	import LanguageWidget from './LanguageWidget.svelte';
	import TimeZoneWidget from './TimeZoneWidget.svelte';

	let { fields: served }: { fields: FieldSpec[] } = $props();
	const unregister = [
		registerWidget('language', LanguageWidget),
		registerWidget('time-zone', TimeZoneWidget)
	];
	const form = untrack(() =>
		createSettingsForm({ id: 'language', fields: served, mode: 'instant' })
	);
	onDestroy(() => {
		form.destroy();
		for (const off of unregister) off();
	});
	$effect(() => {
		const saved = (name: string) => form.field(`preferences.${name}`).saved;
		if (form.status !== 'loading' && form.status !== 'load-error')
			followSaved(
				narrowRegion({
					language: saved('language'),
					time_zone: saved('time_zone'),
					first_day_of_week: saved('first_day_of_week')
				})
			);
	});
</script>

<SettingsForm {form}>
	<div class="flex flex-col gap-6">
		{#each form.specs as spec (spec.key)}<SettingField {form} key={spec.key} />{/each}
	</div>
</SettingsForm>
