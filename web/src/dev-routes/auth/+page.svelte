<script lang="ts">
	// Test-only page for the session-expiry e2e (M-auth): installs the auth handlers the way the
	// shell will (W9), then makes one protected request. A 401 from it must land on
	// /login?next=<this URL>, or on /setup when the instance isn't set up.
	import { onMount } from 'svelte';
	import { api } from '$lib/api/client';
	import { installAuthHandlers } from '$lib/features/auth/session.svelte';

	let loaded = $state(false);

	onMount(() => {
		const uninstall = installAuthHandlers();
		api.get('/filters').then(
			() => (loaded = true),
			() => {}
		);
		return uninstall;
	});
</script>

<div class="flex flex-col gap-4 p-6 text-base-content">
	<h1 class="text-display font-semibold">Session probe</h1>
	{#if loaded}<p role="status">Loaded with a session</p>{/if}
</div>
