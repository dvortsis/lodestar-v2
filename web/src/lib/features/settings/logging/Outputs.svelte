<script lang="ts">
	// The outputs (§10.10.12, §12.1): the console (format, detail), the log file (on, folder, size,
	// files kept, age, cap) with the preview of what the staged settings write, and the Windows Event
	// Log on a Windows service. The fields are the screen's form's; this part only lays them out, and
	// loads after the events, below which it sits.
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { FieldSpec, SettingsForm } from '../form';
	import SettingField from '../form/SettingField.svelte';
	import { loadMessages, messagesLoaded } from '../form/messages';
	import type { DeploymentKind } from './catalog.svelte';
	import Preview from './Preview.svelte';

	let {
		form,
		deployment,
		open = {}
	}: {
		form: SettingsForm;
		deployment?: DeploymentKind;
		/** Disclosures to open. An Advanced field with an error also opens its own, so the error
		 * summary's link to it lands on a shown field. */
		open?: Record<string, boolean>;
	} = $props();

	const GROUPS = [
		{ id: 'console', title: m.settings_logging_console, lead: m.settings_logging_console_lead },
		{ id: 'file', title: m.settings_logging_file, lead: m.settings_logging_file_lead },
		{ id: 'event_log', title: m.settings_logging_event_log, lead: undefined }
	];
	const inGroup = (g: string) => form.specs.filter((s) => s.group === g);
	// The Windows Event Log setting matters only on a Windows service.
	const shown = $derived(
		GROUPS.filter(
			(g) => inGroup(g.id).length && (g.id !== 'event_log' || deployment === 'windows_service')
		)
	);
	let toggled = $state<Record<string, boolean>>({});

	// The outputs' labels are in their own map (`settings.logoutput.*`): the fields render once it's in,
	// so no raw key shows. A map that can't load shows the fields by name, as the engine does.
	const inPlace = messagesLoaded(['logoutput']);
	let labels = $state(inPlace);
	if (!inPlace) void loadMessages(['logoutput']).finally(() => (labels = true));
</script>

{#snippet fields(specs: FieldSpec[])}
	{#each specs as s (s.key)}<SettingField {form} key={s.key} />{/each}
{/snippet}

{#each labels ? shown : [] as g (g.id)}
	{@const specs = inGroup(g.id)}
	{@const advanced = specs.filter((s) => s.advanced)}
	<section class="flex flex-col gap-6" aria-labelledby="logging-{g.id}">
		<div>
			<h2 id="logging-{g.id}" class="text-body-lg font-semibold">{g.title()}</h2>
			{#if g.lead}<p class="text-muted-text">{g.lead()}</p>{/if}
		</div>
		{@render fields(specs.filter((s) => !s.advanced))}
		{#if advanced.length && advanced.length === specs.length}
			<!-- A group of Advanced fields only (the Event Log): nothing to hide them behind. -->
			{@render fields(advanced)}
		{:else if advanced.length}
			<details
				open={toggled[g.id] ||
					open[g.id] ||
					advanced.some((s) => form.field(s.key).error !== undefined)}
				ontoggle={(e) => (toggled[g.id] = e.currentTarget.open)}
			>
				<summary class="ls-focus cursor-pointer py-3 text-label"
					>{m.settings_shell_advanced({
						count: formatCount(advanced.length, currentLocale())
					})}</summary
				>
				<div class="flex flex-col gap-6 pt-4">{@render fields(advanced)}</div>
			</details>
		{/if}
		{#if g.id === 'file'}<Preview {form} />{/if}
	</section>
{/each}
