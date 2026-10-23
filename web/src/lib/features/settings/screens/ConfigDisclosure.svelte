<script lang="ts">
	// "View as config.toml" (§10.10.2): a closed disclosure at the end of a generated screen. Its body
	// loads the first time it opens.
	import { m } from '$lib/paraglide/messages.js';
	import type { Component } from 'svelte';
	import type { FieldSpec, SettingsForm } from '../form';

	let { form, specs }: { form: SettingsForm; specs: FieldSpec[] } = $props();

	type Body = Component<{ form: SettingsForm; specs: FieldSpec[] }>;
	let Body = $state.raw<Body>();
	let open = $state(false);
	function toggled(isOpen: boolean) {
		open = isOpen;
		if (isOpen && !Body)
			void import('./ConfigToml.svelte').then((mod) => (Body = mod.default as unknown as Body));
	}
</script>

<details
	class="config-disclosure"
	ontoggle={(e) => toggled(e.currentTarget.open)}
	data-testid="settings-config-view"
>
	<summary class="config-summary">{m.settings_ui_config_view()}</summary>
	{#if open && Body}<div class="config-body"><Body {form} {specs} /></div>{/if}
</details>

<style>
	.config-summary {
		display: flex;
		align-items: center;
		min-height: var(--control-height);
		cursor: pointer;
	}
	@media (pointer: coarse) {
		.config-summary {
			min-height: var(--target-min);
		}
	}
	.config-body {
		margin-top: calc(var(--spacing) * 2);
	}
</style>
