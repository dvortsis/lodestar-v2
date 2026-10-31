<script lang="ts">
	// Settings → Notifications (§10.10.8): ntfy, Discord, email and webhook, each with **Send test**
	// on the values on screen, and which events go where. The tests load once the page shows.
	import { m } from '$lib/paraglide/messages.js';
	import GroupedForm from './GroupedForm.svelte';
	import Lazy from './Lazy.svelte';
	import SectionScreen from './SectionScreen.svelte';

	const TESTED = ['ntfy', 'discord', 'email', 'webhook'];
</script>

<SectionScreen
	title={m.settings_screen_notifications_title()}
	purpose={m.settings_screen_notifications_purpose()}
	sections={['notifications']}
>
	{#snippet children(fields)}
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
	{/snippet}
</SectionScreen>
