<script lang="ts">
	// Settings → Integrations (§10.10.8): the download-client cards, the default client, and the
	// Torznab endpoint with **Test connection** on the values on screen.
	import { m } from '$lib/paraglide/messages.js';
	import type { FieldSpec } from '../form';
	import ClientSelectWidget from './ClientSelectWidget.svelte';
	import ClientsWidget from './ClientsWidget.svelte';
	import GroupedForm from './GroupedForm.svelte';
	import { given, str } from './staged';
	import TestButton from './TestButton.svelte';

	let { fields }: { fields: FieldSpec[] } = $props();
</script>

<GroupedForm
	id="integrations"
	{fields}
	widgets={{ clients: ClientsWidget, default_client: ClientSelectWidget }}
>
	{#snippet after(group, form)}
		{#if group === 'torznab'}
			<TestButton
				label={m.settings_screen_test_connection()}
				aria-label={m.settings_screen_test_connection_named({ name: 'Torznab' })}
				missing={m.settings_screen_test_need_url()}
				build={() => {
					const url = str(form, 'integrations.torznab_url').trim();
					return url === ''
						? null
						: {
								kind: 'torznab',
								url,
								...given('api_key', str(form, 'integrations.torznab_api_key'))
							};
				}}
				success={(version) =>
					version
						? m.settings_screen_test_connected_version({ version })
						: m.settings_screen_test_connected()}
			/>
		{/if}
	{/snippet}
</GroupedForm>
