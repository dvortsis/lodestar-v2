<script lang="ts">
	// A section's fields in their groups on the form engine (§10.10.4), with room after a group
	// for what only a hand-built screen has: a test button, a danger zone. `widgets` maps a field
	// name to the control that stands in for the JSON fallback of a shape the schema can't say
	// (a list of cards, a choice of channels); the form registers them for its own life.
	import { onDestroy, untrack, type Snippet } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { formatCount } from '$lib/format';
	import { createSettingsForm, groupFields, settingText, type FieldSpec } from '../form';
	import type { SettingsForm as Form } from '../form';
	import SettingField from '../form/SettingField.svelte';
	import SettingsForm from '../form/SettingsForm.svelte';
	import { registerWidget } from '../form/widgets';
	import LazyWidget from './LazyWidget.svelte';

	interface Props {
		id: string;
		fields: FieldSpec[];
		/** Fields shown by a widget of `widgets.ts` instead of the engine's control. */
		widgets?: string[];
		/** Runs once with the form, before it first renders (a screen's gates). */
		setup?: (form: Form) => (() => void) | void;
		after?: Snippet<[group: string, form: Form]>;
		end?: Snippet<[form: Form]>;
	}
	let { id, fields: served, widgets = [], setup, after, end }: Props = $props();

	const names = untrack(() => widgets);
	const widgetName = (field: string) => `${id}.${field}`;
	const unregister = untrack(() =>
		names.map((name) => registerWidget(widgetName(name), LazyWidget))
	);
	const form = untrack(() =>
		createSettingsForm({
			id,
			fields: served.map((spec) =>
				names.includes(spec.name) ? { ...spec, widget: widgetName(spec.name) } : spec
			)
		})
	);
	const teardown = untrack(() => setup?.(form));
	onDestroy(() => {
		teardown?.();
		form.destroy();
		for (const off of unregister) off();
	});
	const groups = $derived(groupFields(form.specs));
</script>

<SettingsForm {form}>
	{#each groups as group (group.group)}
		<section class="flex flex-col gap-6" aria-label={settingText(group.labelKey) ?? group.group}>
			<h2 class="text-body-lg font-semibold">{settingText(group.labelKey) ?? group.group}</h2>
			{#each group.essentials as spec (spec.key)}<SettingField {form} key={spec.key} />{/each}
			{#if group.advanced.length}
				<details>
					<summary class="ls-focus cursor-pointer py-3 text-label"
						>{m.settings_shell_advanced({ count: formatCount(group.advanced.length) })}</summary
					>
					<div class="flex flex-col gap-6 pt-4">
						{#each group.advanced as spec (spec.key)}<SettingField {form} key={spec.key} />{/each}
					</div>
				</details>
			{/if}
			{@render after?.(group.group, form)}
		</section>
	{/each}
	{@render end?.(form)}
</SettingsForm>
