<script lang="ts">
	import { onMount } from 'svelte';
	import { api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import FieldsStep from './FieldsStep.svelte';

	let step: FieldsStep;
	export const save = () => step.save();
	export const needsRestart = () => step.needsRestart();

	// `/system`'s `data.filesystem_warning` names what's wrong and the safe alternative (move to a
	// pool path on Unraid; a local disk for FUSE and network filesystems). It's a warning only: a
	// failed read shows nothing, and Continue is never held back.
	let warning = $state<string | null>(null);
	onMount(() => {
		let live = true;
		api.get('/system').then(
			(s) => live && (warning = s.data.filesystem_warning),
			() => {
				// A failed read shows no warning; Continue is never held back.
			}
		);
		return () => (live = false);
	});
</script>

<!-- Always in the DOM, empty until the warning arrives, so assistive tech hears the change. -->
<div role="status" class="contents" data-testid="wizard-storage-fs">
	{#if warning}<Banner kind="warning">{warning} {m.wizard_storage_fs_note()}</Banner>{/if}
</div>
<FieldsStep
	bind:this={step}
	id="wizard.storage"
	keys={['storage.archive_raw', 'storage.mmap_mib']}
	lead={m.wizard_storage_lead()}
/>
