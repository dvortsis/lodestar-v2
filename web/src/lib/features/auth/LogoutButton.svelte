<script lang="ts">
	// "Sign out" for the shell's menu. A failure leaves the session as it was and says why.
	import Button from '$lib/ui/controls/Button.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { logout } from './session.svelte';

	interface Props {
		variant?: 'secondary' | 'quiet';
		dense?: boolean;
	}

	let { variant = 'quiet', dense = false }: Props = $props();
	let busy = $state(false);

	async function onclick() {
		if (busy) return;
		busy = true;
		try {
			await logout();
		} catch {
			toast.push({ kind: 'error', text: m.auth_logout_failed() });
		} finally {
			busy = false;
		}
	}
</script>

<Button {variant} {dense} loading={busy} {onclick}>{m.auth_logout()}</Button>
