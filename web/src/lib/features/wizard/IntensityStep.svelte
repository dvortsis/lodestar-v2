<script lang="ts">
	import { m } from '$lib/paraglide/messages.js';
	import FieldsStep from './FieldsStep.svelte';

	let step: FieldsStep;
	export const save = () => step.save();
</script>

<FieldsStep
	bind:this={step}
	id="wizard.intensity"
	keys={['crawler.intensity']}
	lead={m.wizard_intensity_lead()}
/>
