<script lang="ts">
	import { m } from '$lib/paraglide/messages.js';
	import SectionScreen from './SectionScreen.svelte';
</script>

<SectionScreen
	title={m.settings_screen_notifications_title()}
	purpose={m.settings_screen_notifications_purpose()}
	sections={['notifications']}
	form={() => import('./NotificationsForm.svelte')}
/>
