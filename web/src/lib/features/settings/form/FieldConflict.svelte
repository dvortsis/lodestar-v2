<script lang="ts">
	// A field's conflict (§10.10.5): what's saved now, Keep mine or Use theirs. Loaded by
	// `SettingField.svelte` with the first conflict; its row and icon are the field's.
	import Button from '$lib/ui/controls/Button.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { displayValue } from './display';
	import type { SettingsForm } from './form.svelte';
	import type { FieldKey } from './types';

	let { form, key }: { form: SettingsForm; key: FieldKey } = $props();

	const f = $derived(form.field(key));
</script>

{#if f.conflict}
	<span data-testid="conflict-text"
		>{m.settings_form_conflict({ value: displayValue(f.spec, f.conflict.theirs) })}</span
	>
	<Button dense onclick={() => form.resolve(key, 'mine')}>{m.settings_form_keep_mine()}</Button>
	<Button dense onclick={() => form.resolve(key, 'theirs')}>{m.settings_form_use_theirs()}</Button>
{/if}
