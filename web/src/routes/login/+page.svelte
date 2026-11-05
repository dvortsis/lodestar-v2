<script lang="ts">
	import { page } from '$app/state';
	import AuthGate from '$lib/features/auth/AuthGate.svelte';
	import AuthPage from '$lib/features/auth/AuthPage.svelte';
	import LoginForm from '$lib/features/auth/LoginForm.svelte';
	import NoticeGate from '$lib/features/auth/NoticeGate.svelte';
	import { safeNext } from '$lib/features/auth/next';
	import { m } from '$lib/paraglide/messages.js';

	const next = $derived(safeNext(page.url.searchParams.get('next'), page.url.origin));
	// Signed in, but the Legal Notice must be accepted first (owner decision §2.38).
	let notice = $state(false);
	// Handed over by signing in: focus follows to the notice (not on first paint).
	let handover = $state(false);
	const title = $derived(notice ? m.auth_notice_title() : m.auth_login_title());
</script>

<svelte:head><title>{title} · Lodestar</title></svelte:head>

<AuthPage {title} lead={notice ? m.auth_notice_lead() : m.auth_login_lead()}>
	<AuthGate page="login" {next} onstatus={(s) => (notice = s?.notice_required === true)}>
		{#if notice}
			<NoticeGate {next} focusOnLoad={handover} />
		{:else}
			<LoginForm {next} onnotice={() => ((handover = true), (notice = true))} />
		{/if}
	</AuthGate>
</AuthPage>
