<script lang="ts">
	import { onDestroy, onMount, tick, untrack } from 'svelte';
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { m } from '$lib/paraglide/messages.js';
	import { formatCount } from '$lib/format';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import IconRestart from '~icons/ph/arrows-clockwise';
	import {
		SettingField,
		SettingsForm,
		createSettingsForm,
		focusSettingField,
		groupFields,
		loadControls,
		loadMessages,
		messagesLoaded,
		settingText,
		widgetFor,
		type FieldKey,
		type FieldSpec
	} from '../form/embed';
	import type { Screen } from './screens';
	let { screen, fields }: { screen: Screen; fields: FieldSpec[] } = $props();
	// The parent keys this component by screen: no draft, request or save survives navigation.
	const form = untrack(() =>
		createSettingsForm({
			id: screen.id,
			fields,
			href: (key) => resolve(`/settings/${screen.id}#${encodeURIComponent(key)}` as '/search')
		})
	);
	onDestroy(() => form.destroy());
	let changedOnly = $state(false);
	let expanded = $state<Record<string, boolean>>({});
	const preferenceKey = untrack(() => `lodestar.settings.advanced.${screen.id}`);
	onMount(() => {
		try {
			const saved: unknown = JSON.parse(localStorage.getItem(preferenceKey) ?? '{}');
			if (saved && typeof saved === 'object' && !Array.isArray(saved)) {
				expanded = Object.fromEntries(Object.entries(saved).filter(([, value]) => value === true));
			}
		} catch {
			/* A blocked or malformed preference never blocks settings. */
		}
	});
	function toggle(group: string, open: boolean) {
		expanded = { ...expanded, [group]: open };
		try {
			localStorage.setItem(preferenceKey, JSON.stringify(expanded));
		} catch {
			/* Best effort. */
		}
	}
	const visible = $derived(
		form.specs.filter(
			(spec) => !changedOnly || form.field(spec.key).changed || form.field(spec.key).dirty
		)
	);
	const groups = $derived(groupFields(visible));
	async function reveal(key: FieldKey) {
		const spec = form.specs.find((spec) => spec.key === key);
		if (!spec) return;
		changedOnly = false;
		if (spec.advanced) toggle(spec.group, true);
		await loadControls([widgetFor(spec)]);
		await tick();
	}
	let focusGeneration = 0;
	// The fields render once their labels are in (`SettingsForm`), so a link to one waits for that.
	const sections = untrack(() => fields.map((spec) => spec.section));
	const inPlace = messagesLoaded(sections);
	let labelsReady = $state(inPlace);
	if (!inPlace) void loadMessages(sections).then(() => (labelsReady = true));
	const ready = $derived(labelsReady && form.status !== 'loading' && form.status !== 'load-error');
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
			if (generation !== focusGeneration) return;
			focusSettingField(key);
		});
		return () => {
			++focusGeneration;
		};
	});
</script>

<SettingsForm {form} onReveal={reveal}>
	{#if form.restartPending.length}
		<Banner
			><span class="flex items-center gap-2"
				><IconRestart aria-hidden="true" /><span data-testid="settings-restart-count"
					>{m.settings_shell_restart({ count: formatCount(form.restartPending.length) })}</span
				></span
			></Banner
		>
	{/if}
	<Toggle label={m.settings_shell_changed_only()} bind:checked={changedOnly} />
	{#if !visible.length}<p role="status">{m.settings_shell_no_changed()}</p>{/if}
	{#each groups as group (group.group)}
		<section class="flex flex-col gap-6" aria-label={settingText(group.labelKey) ?? group.group}>
			<h2 class="text-body-lg font-semibold">{settingText(group.labelKey) ?? group.group}</h2>
			{#each group.essentials as spec (spec.key)}<SettingField {form} key={spec.key} />{/each}
			{#if group.advanced.length}
				<details
					open={expanded[group.group] === true}
					ontoggle={(event) => toggle(group.group, event.currentTarget.open)}
				>
					<summary
						class="ls-focus cursor-pointer py-3 text-label"
						data-testid="settings-advanced-count"
						>{m.settings_shell_advanced({ count: formatCount(group.advanced.length) })}</summary
					>
					<div class="flex flex-col gap-6 pt-4">
						{#each group.advanced as spec (spec.key)}<SettingField {form} key={spec.key} />{/each}
					</div>
				</details>
			{/if}
		</section>
	{/each}
</SettingsForm>
