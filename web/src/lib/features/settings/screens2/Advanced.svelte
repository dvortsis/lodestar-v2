<script lang="ts">
	// A group's Advanced fields (§10.10.4) behind a closed disclosure. They load after the group's
	// essentials, since nothing in them is needed to paint the screen.
	import { formatCount } from '$lib/format';
	import { m } from '$lib/paraglide/messages.js';
	import type { FieldSpec, SettingsForm } from '../form';
	import SettingField from '../form/SettingField.svelte';

	let { form, specs }: { form: SettingsForm; specs: FieldSpec[] } = $props();
</script>

<details>
	<summary class="ls-focus cursor-pointer py-3 text-label"
		>{m.settings_shell_advanced({ count: formatCount(specs.length) })}</summary
	>
	<div class="flex flex-col gap-6 pt-4">
		{#each specs as spec (spec.key)}<SettingField {form} key={spec.key} />{/each}
	</div>
</details>
