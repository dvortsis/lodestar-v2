<script lang="ts">
	import AboutPage from '$lib/features/settings/about/AboutPage.svelte';
	import { m } from '$lib/paraglide/messages.js';
</script>

<svelte:head><title>{m.auth_about_title()} · Lodestar</title></svelte:head>

<AboutPage />
