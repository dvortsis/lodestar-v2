<script lang="ts">
	// Settings → Integrations (§10.10.8): the download-client cards, the default client, and the
	// Torznab endpoint with **Test connection** on the values on screen. The cards and the test
	// load once the fields are on the page.
	import type { FieldSpec } from '../form';
	import GroupedForm from './GroupedForm.svelte';
	import Lazy from './Lazy.svelte';

	let { fields }: { fields: FieldSpec[] } = $props();
</script>

<GroupedForm id="integrations" {fields} widgets={['clients', 'default_client']}>
	{#snippet after(group, form)}
		{#if group === 'torznab'}
			<Lazy load={() => import('./GroupTest.svelte')} props={{ form, group }} />
		{/if}
	{/snippet}
</GroupedForm>
