<script lang="ts">
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { loadSettingsIndex, type SettingsIndexView } from '../form/schema';
	import { screens, screenFields } from './screens';
	import SectionForm from './SectionForm.svelte';
	import SettingsExport from './SettingsExport.svelte';
	let { section }: { section: string } = $props();
	let index = $state<SettingsIndexView>();
	let failed = $state(false);
	let attempt = $state(0);
	const screen = $derived(screens.find((screen) => screen.id === section));
	const fields = $derived(screen && index ? screenFields(screen, index) : []);
	$effect(() => {
		void attempt;
		if (!screen) return;
		let live = true;
		failed = false;
		void loadSettingsIndex().then(
			(value) => {
				if (live) index = value;
			},
			() => {
				if (live) failed = true;
			}
		);
		return () => {
			live = false;
		};
	});
</script>

<svelte:head><title>{screen?.label() ?? m.shell_nav_settings()} · Lodestar</title></svelte:head>
<h1 class="text-title font-semibold">{screen?.label() ?? m.shell_nav_settings()}</h1>
{#if !screen}
	<p>{m.settings_shell_unavailable()}</p>
{:else}
	<p class="text-muted-text">{screen.purpose()}</p>
	{#if failed}
		<Banner kind="error"
			>{m.settings_form_load_error()}{#snippet action()}<Button dense onclick={() => attempt++}
					>{m.settings_form_retry()}</Button
				>{/snippet}</Banner
		>
	{:else if !index}
		<p aria-busy="true" role="status">{m.settings_form_loading()}</p>
	{:else if !fields.length}
		<p>{m.settings_shell_server_unavailable()}</p>
	{:else}
		{#if screen.id === 'network'}<Banner>{m.settings_shell_network_readonly()}</Banner>{/if}
		{#if fields.some((field) => field.danger && field.danger !== 'disruptive')}<Banner
				>{m.settings_shell_guarded_readonly()}</Banner
			>{/if}
		{#key screen.id}<SectionForm {screen} {fields} />{/key}
		{#if screen.id === 'general'}<SettingsExport />{/if}
	{/if}
{/if}
