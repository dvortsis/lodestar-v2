<script lang="ts">
	// "Show keys" from the field menu: the setting's config-file key and environment variable.
	// Part of the menu's chunk (`menu.ts`).
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { FieldSpec } from './types';

	let { spec }: { spec: FieldSpec } = $props();
</script>

<dl class="grid grid-cols-2 gap-x-4 gap-y-1 text-small" data-testid="field-keys">
	{#if spec.config}
		<dt class="text-muted-text">{m.settings_form_config_key()}</dt>
		<dd><Mono>{spec.config}</Mono></dd>
	{/if}
	{#if spec.env}
		<dt class="text-muted-text">{m.settings_form_env_var()}</dt>
		<dd><Mono>{spec.env}</Mono></dd>
	{/if}
</dl>
