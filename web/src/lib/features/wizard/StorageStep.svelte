<script lang="ts">
	import { onMount } from 'svelte';
	import { api, isAbort } from '$lib/api/client';
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
			(e) => void (isAbort(e) || e)
		);
		return () => (live = false);
	});
</script>

{#if warning}
	<div data-testid="wizard-storage-fs">
		<Banner kind="warning">{warning} {m.wizard_storage_fs_note()}</Banner>
	</div>
{/if}
<FieldsStep
	bind:this={step}
	id="wizard.storage"
	keys={['storage.archive_raw', 'storage.mmap_mib']}
	lead={m.wizard_storage_lead()}
/>
