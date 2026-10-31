<script lang="ts">
	// Settings → Insights & digest (§10.10.8): the digest's schedule and channels, query logging
	// behind its Privacy dialog (§10.10.7), retention, and Clear usage data. The digest preview
	// (§10.9.7) mounts at `data-slot="digest-preview"`, built by S-insights-3.
	import type { FieldSpec } from '../form';
	import GroupedForm from './GroupedForm.svelte';
	import Lazy from './Lazy.svelte';

	let { fields }: { fields: FieldSpec[] } = $props();
</script>

<GroupedForm
	id="insights"
	{fields}
	widgets={['digest_channels']}
	setup={(form) =>
		// Turning it off asks nothing. Turning it on says what is recorded, and Cancel keeps the save.
		form.gate('privacy', async () => {
			if (form.field('insights.query_logging').value !== true) return true;
			const days = form.field('insights.query_retention_days').value;
			const { confirmQueryLogging } = await import('./privacy');
			return confirmQueryLogging(typeof days === 'number' ? days : 30);
		})}
>
	{#snippet after(group)}
		{#if group === 'digest'}
			<div data-slot="digest-preview"></div>
		{:else if group === 'usage'}
			<Lazy load={() => import('./ClearUsage.svelte')} />
		{/if}
	{/snippet}
</GroupedForm>
