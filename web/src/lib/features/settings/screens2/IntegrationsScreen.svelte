<script lang="ts">
	// Settings → Integrations (§10.10.8): the download-client cards, the default client, and the
	// Torznab endpoint with **Test connection** on the values on screen. The clients and the test
	// load once the fields are on the page.
	import { m } from '$lib/paraglide/messages.js';
	import GroupedForm from './GroupedForm.svelte';
	import Lazy from './Lazy.svelte';
	import SectionScreen from './SectionScreen.svelte';
</script>

<SectionScreen
	title={m.settings_screen_integrations_title()}
	purpose={m.settings_screen_integrations_purpose()}
	sections={['integrations']}
>
	{#snippet children(fields)}
		<GroupedForm id="integrations" {fields} widgets={['clients', 'default_client']}>
			{#snippet after(group, form)}
				{#if group === 'torznab'}
					<Lazy load={() => import('./GroupTest.svelte')} props={{ form, group }} />
				{/if}
			{/snippet}
		</GroupedForm>
	{/snippet}
</SectionScreen>
