<script lang="ts">
	// The Language & region form: the engine in instant mode (Preferences apply at once, with
	// Undo, §10.10.1). A saved language is applied to the UI here; applying it at boot, and the
	// time zone and first day of the week where they're read, are follow-ups (decisions file).
	import { onDestroy, untrack } from 'svelte';
	import { initLocale } from '$lib/i18n/locale.svelte';
	import { createSettingsForm, type FieldSpec } from '../form';
	import SettingField from '../form/SettingField.svelte';
	import SettingsForm from '../form/SettingsForm.svelte';
	import { registerWidget } from '../form/widgets';
	import LanguageWidget from './LanguageWidget.svelte';
	import TimeZoneWidget from './TimeZoneWidget.svelte';

	let { fields: served }: { fields: FieldSpec[] } = $props();
	// Automatic is these widgets' own first choice: an empty value follows the browser. The engine's
	// Automatic switch can't turn off for an empty text in instant mode, so the spec drops `x-auto`.
	const fields = untrack(() => served.map((spec) => ({ ...spec, auto: undefined })));
	const unregister = [
		registerWidget('language', LanguageWidget),
		registerWidget('time-zone', TimeZoneWidget)
	];
	const form = untrack(() => createSettingsForm({ id: 'language', fields, mode: 'instant' }));
	onDestroy(() => {
		form.destroy();
		for (const off of unregister) off();
	});
	$effect(() => {
		const saved = form.field('preferences.language').saved;
		if (form.status !== 'loading' && form.status !== 'load-error')
			initLocale(typeof saved === 'string' ? saved : null);
	});
</script>

<SettingsForm {form}>
	<div class="flex flex-col gap-6">
		{#each form.specs as spec (spec.key)}<SettingField {form} key={spec.key} />{/each}
	</div>
</SettingsForm>
