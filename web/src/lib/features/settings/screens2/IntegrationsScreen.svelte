<script lang="ts">
	import { m } from '$lib/paraglide/messages.js';
	import SectionScreen from './SectionScreen.svelte';
</script>

<SectionScreen
	title={m.settings_screen_integrations_title()}
	purpose={m.settings_screen_integrations_purpose()}
	sections={['integrations']}
	form={() => import('./IntegrationsForm.svelte')}
/>
