<script lang="ts">
	import { m } from '$lib/paraglide/messages.js';
	import SectionScreen from './SectionScreen.svelte';
</script>

<SectionScreen
	title={m.settings_screen_insights_title()}
	purpose={m.settings_screen_insights_purpose()}
	sections={['insights']}
	form={() => import('./InsightsForm.svelte')}
/>
