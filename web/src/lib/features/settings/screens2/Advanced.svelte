<script lang="ts">
	// A group's Advanced fields (§10.10.4) behind a closed disclosure. They load after the group's
	// essentials, since nothing in them is needed to paint the screen.
	import { formatCount } from '$lib/format';
	import { m } from '$lib/paraglide/messages.js';
	import type { FieldSpec, SettingsForm } from '../form';
	import SettingField from '../form/SettingField.svelte';

	let {
		form,
		specs,
		open = false,
		ontoggle
	}: {
		form: SettingsForm;
		specs: FieldSpec[];
		open?: boolean;
		/** The disclosure was opened or closed by the user. */
		ontoggle?: (open: boolean) => void;
	} = $props();
</script>

<details {open} ontoggle={(event) => ontoggle?.(event.currentTarget.open)}>
	<summary class="ls-focus cursor-pointer py-3 text-small"
		>{m.settings_shell_advanced({ count: formatCount(specs.length) })}</summary
	>
	<div class="flex flex-col gap-6 pt-4">
		{#each specs as spec (spec.key)}<SettingField {form} key={spec.key} />{/each}
	</div>
</details>
