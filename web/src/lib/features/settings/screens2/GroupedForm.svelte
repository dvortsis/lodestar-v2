<script lang="ts">
	// A section's fields in their groups on the form engine (§10.10.4), with room after a group
	// for what only a hand-built screen has: a test button, a danger zone. `widgets` maps a field
	// name to the control that stands in for the JSON fallback of a shape the schema can't say
	// (a list of cards, a choice of channels); the form registers them for its own life.
	import { onDestroy, tick, untrack, type Snippet } from 'svelte';
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { createSettingsForm, groupFields, settingText, type FieldSpec } from '../form';
	import type { SettingsForm as Form } from '../form';
	import SettingField from '../form/SettingField.svelte';
	import SettingsForm from '../form/SettingsForm.svelte';
	import { loadControls } from '../form/controls.svelte';
	import { focusSettingField } from '../form/focus';
	import { loadMessages, messagesLoaded } from '../form/messages';
	import { registerWidget, widgetFor } from '../form/widgets';
	import type { FieldKey } from '../form/types';
	import Lazy from './Lazy.svelte';
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
			href: (key) => resolve(`/settings/${id}#${encodeURIComponent(key)}` as '/search'),
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

	// A link to a field (the URL hash, Copy link, the error summary) reveals it: an Advanced field
	// opens its disclosure, a lazy control loads. The field renders once its labels are in.
	let expanded = $state<Record<string, boolean>>({});
	async function reveal(key: FieldKey) {
		const spec = form.specs.find((spec) => spec.key === key);
		if (!spec) return;
		if (spec.advanced) {
			expanded[spec.group] = true;
			await import('./Advanced.svelte');
		}
		await loadControls([widgetFor(spec)]);
		await tick();
	}
	const sections = untrack(() => served.map((spec) => spec.section));
	const inPlace = messagesLoaded(sections);
	let labelsReady = $state(inPlace);
	if (!inPlace) void loadMessages(sections).then(() => (labelsReady = true));
	const ready = $derived(labelsReady && form.status !== 'loading' && form.status !== 'load-error');
	let focusGeneration = 0;
	$effect(() => {
		const hash = page.url.hash;
		const generation = ++focusGeneration;
		if (!hash || !ready) return;
		let key: FieldKey;
		try {
			key = decodeURIComponent(hash.slice(1)) as FieldKey;
		} catch {
			return;
		}
		if (!form.specs.some((spec) => spec.key === key)) return;
		void untrack(() => reveal(key)).then(() => {
			if (generation === focusGeneration) focusSettingField(key);
		});
		return () => {
			++focusGeneration;
		};
	});
</script>

<SettingsForm {form} onReveal={reveal}>
	{#each groups as group (group.group)}
		<section class="flex flex-col gap-6" aria-label={settingText(group.labelKey) ?? group.group}>
			<h2 class="text-body-lg font-semibold">{settingText(group.labelKey) ?? group.group}</h2>
			{#each group.essentials as spec (spec.key)}<SettingField {form} key={spec.key} />{/each}
			{#if group.advanced.length}
				<Lazy
					load={() => import('./Advanced.svelte')}
					props={{
						form,
						specs: group.advanced,
						open: expanded[group.group] === true,
						ontoggle: (open: boolean) => (expanded[group.group] = open)
					}}
				/>
			{/if}
			{@render after?.(group.group, form)}
		</section>
	{/each}
	{@render end?.(form)}
</SettingsForm>
