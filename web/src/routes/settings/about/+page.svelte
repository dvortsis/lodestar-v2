<script lang="ts">
	import AboutPage from '$lib/features/settings/about/AboutPage.svelte';
	import { m } from '$lib/paraglide/messages.js';
</script>

<svelte:head><title>{m.shell_more_about()} · Lodestar</title></svelte:head>

<AboutPage />
