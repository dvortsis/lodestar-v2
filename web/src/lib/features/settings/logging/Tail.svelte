<script lang="ts">
	// What follows the data kinds on Settings → Logging (§10.10.12's order): the outputs with the
	// preview, then the log files and the Danger zone. Each loads on its own after the first view.
	import { m } from '$lib/paraglide/messages.js';
	import type { SettingsForm } from '../form';
	import type { LoggingCatalog } from './catalog.svelte';

	let { catalog, form }: { catalog: LoggingCatalog; form?: SettingsForm } = $props();

	let Outputs = $state.raw<typeof import('./Outputs.svelte').default>();
	let Files = $state.raw<typeof import('./LogFiles.svelte').default>();
	let failed = $state(false);
	const fail = () => (failed = true);
	import('./Outputs.svelte').then((mod) => (Outputs = mod.default), fail);
	import('./LogFiles.svelte').then((mod) => (Files = mod.default), fail);
</script>

{#if Outputs && form}<Outputs {form} deployment={catalog.deployment} />{/if}
{#if Files}<Files {catalog} />{/if}
{#if failed}<p role="alert" class="text-small text-error">{m.settings_form_chunk_failed()}</p>{/if}
