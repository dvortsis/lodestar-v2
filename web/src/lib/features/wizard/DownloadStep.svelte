<script lang="ts">
	// Step 7: a download client (§10.10.10, §9, §18.10). The same field, card widget and Test
	// connection as Settings → Integrations, on the engine through `form/embed.ts`. qBittorrent is
	// the recommended client. Skip sends nothing.
	import { onDestroy, untrack } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import type { FieldSpec } from '../settings/form/embed';
	import { loadSettingsIndex } from '../settings/form/schema';
	import { registerWidget } from '../settings/form/widgets';
	import LazyWidget from '../settings/screens2/LazyWidget.svelte';
	import FieldsStep from './FieldsStep.svelte';

	const KEY = 'integrations.clients';
	let specs = $state<FieldSpec[]>();
	let failed = $state(false);
	let step = $state<FieldsStep>();

	/** True when the shell may move on. False while the setting isn't loaded: nothing was shown. */
	export const save = async () => (step ? step.save() : false);
	const off = untrack(() => registerWidget(KEY, LazyWidget));
	onDestroy(off);
	void loadSettingsIndex().then(
		(index) => {
			const spec = index.fieldsOf(['integrations']).find((s) => s.key === KEY);
			if (spec) specs = [{ ...spec, widget: KEY }];
			else failed = true;
		},
		() => (failed = true)
	);
</script>

{#if failed}
	<p class="text-body">{m.wizard_download_lead()}</p>
	<Banner kind="error">{m.wizard_download_load_failed()}</Banner>
{:else if specs}
	<FieldsStep bind:this={step} id="wizard.download" keys={specs} lead={m.wizard_download_lead()} />
{:else}
	<p class="text-body">{m.wizard_download_lead()}</p>
	<p class="text-muted-text" aria-busy="true">{m.wizard_loading()}</p>
{/if}
