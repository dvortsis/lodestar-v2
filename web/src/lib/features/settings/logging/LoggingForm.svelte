<script lang="ts">
	// Settings → Logging's staged settings on the form engine (§10.10.4): the Verbosity preset,
	// the event list, per-component levels under Advanced and the Libraries filter under Expert.
	// They're Safe changes (§10.10.7): staged, saved, Undo. The event list and the component levels
	// stand in for the JSON fallback and load when their fields are on the page.
	import { onDestroy, untrack } from 'svelte';
	import { resolve } from '$app/paths';
	import { m } from '$lib/paraglide/messages.js';
	import { createSettingsForm, type FieldSpec, type SettingsForm as Form } from '../form';
	import SettingField from '../form/SettingField.svelte';
	import SettingsForm from '../form/SettingsForm.svelte';
	import { loadMessages, messagesLoaded } from '../form/messages';
	import { registerWidget } from '../form/widgets';
	import type { FieldKey } from '../form/types';
	import WidgetSlot from './WidgetSlot.svelte';

	interface Props {
		fields: FieldSpec[];
		/** Hands the form to the screen, so a privacy change can merge the new revision. */
		onform?: (form: Form) => void;
	}
	let { fields: served, onform }: Props = $props();

	const WIDGETS = ['verbosity', 'events', 'components'];
	const name = (field: string) => `logging.${field}`;
	const unregister = WIDGETS.map((field) => registerWidget(name(field), WidgetSlot));
	const form = untrack(() =>
		createSettingsForm({
			id: 'logging',
			href: (key) => resolve(`/settings/logging#${encodeURIComponent(key)}` as '/search'),
			fields: served.map((spec) =>
				WIDGETS.includes(spec.name) ? { ...spec, widget: name(spec.name) } : spec
			)
		})
	);
	untrack(() => onform?.(form));
	onDestroy(() => {
		form.destroy();
		for (const off of unregister) off();
	});

	const spec = (n: string) => form.specs.find((s) => s.name === n);
	const essentials = $derived(form.specs.filter((s) => !s.advanced));
	const advanced = $derived(spec('components'));
	const expert = $derived(spec('libraries'));

	let open = $state<Record<string, boolean>>({});
	function reveal(key: FieldKey) {
		if (key === advanced?.key) open.advanced = true;
		if (key === expert?.key) open.expert = true;
	}

	const inPlace = messagesLoaded(['logging']);
	let labelsReady = $state(inPlace);
	if (!inPlace) void loadMessages(['logging']).then(() => (labelsReady = true));
</script>

{#if labelsReady}
	<SettingsForm {form} onReveal={reveal}>
		<section class="flex flex-col gap-6" aria-labelledby="logging-events">
			<div>
				<h2 id="logging-events" class="text-body-lg font-semibold">
					{m.settings_logging_events_title()}
				</h2>
				<p class="text-muted-text">{m.settings_logging_events_lead()}</p>
			</div>
			{#each essentials as s (s.key)}<SettingField {form} key={s.key} />{/each}
			{#if advanced}
				<details open={open.advanced} ontoggle={(e) => (open.advanced = e.currentTarget.open)}>
					<summary class="ls-focus cursor-pointer py-3 text-label"
						>{m.settings_logging_advanced()}</summary
					>
					<div class="pt-4"><SettingField {form} key={advanced.key} /></div>
				</details>
			{/if}
			{#if expert}
				<details open={open.expert} ontoggle={(e) => (open.expert = e.currentTarget.open)}>
					<summary class="ls-focus cursor-pointer py-3 text-label"
						>{m.settings_logging_expert()}</summary
					>
					<div class="pt-4"><SettingField {form} key={expert.key} /></div>
				</details>
			{/if}
		</section>
	</SettingsForm>
{:else}
	<p aria-busy="true" role="status">{m.settings_form_loading()}</p>
{/if}
