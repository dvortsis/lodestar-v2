<script lang="ts">
	// Settings → Logging's staged settings on the form engine (§10.10.4): the events (Verbosity, the
	// event list, per-component levels under Advanced, the Libraries filter under Expert) and the
	// outputs (console, log file, Windows Event Log on a Windows service). They're Safe changes
	// (§10.10.7): staged, saved, Undo; the console's detail asks first (Privacy). The widgets, the
	// preview and the gate's dialog load on demand.
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
	// The console's detail is a Privacy change: saving "same as the file" asks first.
	const ungate = form.gate('privacy', async () =>
		(await import('./outputs')).confirmConsoleDetail(form)
	);
	onDestroy(() => {
		ungate();
		form.destroy();
		for (const off of unregister) off();
	});

	const spec = (n: string) => form.specs.find((s) => s.name === n);
	const essentials = $derived(form.specs.filter((s) => s.group === 'events' && !s.advanced));
	const advanced = $derived(spec('components'));
	const expert = $derived(spec('libraries'));

	let open = $state<Record<string, boolean>>({});
	function reveal(key: FieldKey) {
		const s = form.specs.find((x) => x.key === key);
		if (s?.advanced) open[s.name === 'libraries' ? 'expert' : s.group] = true;
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
				<details open={open.events} ontoggle={(e) => (open.events = e.currentTarget.open)}>
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
