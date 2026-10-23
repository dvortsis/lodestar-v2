<script lang="ts">
	// `/settings` (§10.10.3). Part 1: the heading and, below x-large, the grouped list; the
	// "Needs attention" and "Changed from default" sections arrive in part 2.
	import { afterNavigate } from '$app/navigation';
	import { m } from '$lib/paraglide/messages.js';
	import { focusHeading } from './focus';
	import SettingsNav from './SettingsNav.svelte';
	import { useSettings } from './state.svelte';

	const settings = useSettings();
	afterNavigate(() => {
		focusHeading();
	});
</script>

<div class="flex flex-col gap-6" data-testid="settings-overview">
	<header class="flex flex-col gap-2">
		<h1 tabindex="-1" class="text-display font-semibold outline-none">{m.settings_ui_title()}</h1>
		<p class="text-body text-muted-text">{m.settings_ui_overview_purpose()}</p>
	</header>
	{#if !settings.wide}
		<SettingsNav screens={settings.registry.screens} />
	{/if}
</div>
