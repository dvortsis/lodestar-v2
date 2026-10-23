<script lang="ts">
	// What the last save did to a field (§10.10.4's status line): "Saved" (or "applies after a
	// restart"), or in instant mode "Changed" with its Undo. Loaded with the first save.
	import Button from '$lib/ui/controls/Button.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import IconCheck from '~icons/ph/check-circle';
	import type { SettingsForm } from './form.svelte';
	import type { FieldKey } from './types';

	let { form, key, part }: { form: SettingsForm; key: FieldKey; part: 'saved' | 'undo' } = $props();

	const f = $derived(form.field(key));
</script>

{#if part === 'saved'}
	<span class="inline-flex items-center gap-1 text-success">
		<IconCheck class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
		<span class="text-base-content">
			{f.savedRecently === 'restart' ? m.settings_form_saved_restart() : m.settings_form_saved()}
		</span>
	</span>
{:else}
	<span class="inline-flex items-center gap-1">
		<IconCheck class="size-(--icon-size-row) shrink-0 text-success" aria-hidden="true" />
		<span class="text-base-content">{m.settings_form_announce_changed()}</span>
		<Button dense variant="quiet" onclick={() => form.undo(key)}>{m.settings_form_undo()}</Button>
	</span>
{/if}
