<script lang="ts">
	// A field's conflict (§10.10.5): what's saved now, Keep mine or Use theirs, in its own row. It
	// loads with the edit chunk, so the row and its icon stay off the first view.
	import Button from '$lib/ui/controls/Button.svelte';
	import IconWarning from '~icons/ph/warning';
	import { m } from '$lib/paraglide/messages.js';
	import { displayValue } from './display';
	import type { SettingsForm } from './form.svelte';
	import type { FieldKey } from './types';

	let { form, key }: { form: SettingsForm; key: FieldKey } = $props();

	const f = $derived(form.field(key));
</script>

{#if f.conflict}
	<div class="flex flex-wrap items-center gap-2 text-small" data-testid="conflict">
		<IconWarning class="size-(--icon-size-row) shrink-0 text-warning" aria-hidden="true" />
		<span data-testid="conflict-text"
			>{m.settings_form_conflict({ value: displayValue(f.spec, f.conflict.theirs) })}</span
		>
		<Button dense onclick={() => form.resolve(key, 'mine')}>{m.settings_form_keep_mine()}</Button>
		<Button dense onclick={() => form.resolve(key, 'theirs')}>{m.settings_form_use_theirs()}</Button
		>
	</div>
{/if}
