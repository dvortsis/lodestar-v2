<script lang="ts">
	import { m } from '$lib/paraglide/messages.js';
	import FieldsStep from './FieldsStep.svelte';

	let step: FieldsStep;
	export const save = () => step.save();
	export const needsRestart = () => step.needsRestart();
</script>

<FieldsStep
	bind:this={step}
	id="wizard.storage"
	keys={['storage.archive_raw', 'storage.mmap_mib']}
	lead={m.wizard_storage_lead()}
/>
