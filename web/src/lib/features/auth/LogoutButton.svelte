<script lang="ts">
	// "Sign out" as a button (Settings → Security → Sessions). A failure leaves the session as it
	// was and says why (`signOut`).
	import Button from '$lib/ui/controls/Button.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { signOut } from './sign-out';

	interface Props {
		variant?: 'secondary' | 'quiet';
		dense?: boolean;
		testid?: string;
	}

	let { variant = 'quiet', dense = false, testid }: Props = $props();
	let busy = $state(false);

	async function onclick() {
		if (busy) return;
		busy = true;
		await signOut();
		busy = false;
	}
</script>

<Button {variant} {dense} loading={busy} {onclick} data-testid={testid}>{m.auth_logout()}</Button>
