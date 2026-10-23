<script lang="ts">
	import { page } from '$app/state';
	import AuthGate from '$lib/features/auth/AuthGate.svelte';
	import AuthPage from '$lib/features/auth/AuthPage.svelte';
	import LoginForm from '$lib/features/auth/LoginForm.svelte';
	import { safeNext } from '$lib/features/auth/next';
	import { m } from '$lib/paraglide/messages.js';

	const next = $derived(safeNext(page.url.searchParams.get('next'), page.url.origin));
</script>

<svelte:head><title>{m.auth_login_title()} · Lodestar</title></svelte:head>

<AuthPage title={m.auth_login_title()} lead={m.auth_login_lead()}>
	<AuthGate page="login" {next}>
		<LoginForm {next} />
	</AuthGate>
</AuthPage>
