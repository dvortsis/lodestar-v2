<script lang="ts">
	import AuthGate from '$lib/features/auth/AuthGate.svelte';
	import AuthPage from '$lib/features/auth/AuthPage.svelte';
	import SetupForm from '$lib/features/auth/SetupForm.svelte';
	import { m } from '$lib/paraglide/messages.js';
</script>

<svelte:head><title>{m.auth_setup_title()} · Lodestar</title></svelte:head>

<AuthPage title={m.auth_setup_title()} lead={m.auth_setup_lead()}>
	<AuthGate page="setup">
		<div class="flex flex-col gap-4">
			<!-- LEGAL-2: the crawler is held until setup completes. -->
			<p class="text-body text-muted-text">{m.auth_setup_crawler_waits()}</p>
			<SetupForm />
		</div>
	</AuthGate>
</AuthPage>
