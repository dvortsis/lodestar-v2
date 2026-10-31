<script lang="ts">
	// Settings → Notifications (§10.10.8): ntfy, Discord, email and webhook, each with **Send test**
	// on the values on screen, and which events go where. The tests load once the page shows.
	import type { FieldSpec } from '../form';
	import GroupedForm from './GroupedForm.svelte';
	import Lazy from './Lazy.svelte';

	let { fields }: { fields: FieldSpec[] } = $props();
	const TESTED = ['ntfy', 'discord', 'email', 'webhook'];
</script>

<GroupedForm
	id="notifications"
	{fields}
	widgets={['route_alerts', 'route_updates', 'route_health']}
>
	{#snippet after(group, form)}
		{#if TESTED.includes(group)}
			<Lazy load={() => import('./GroupTest.svelte')} props={{ form, group }} />
		{/if}
	{/snippet}
</GroupedForm>
