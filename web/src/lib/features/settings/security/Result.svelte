<script lang="ts">
	// One action's outcome beside its button: icon and text, never colour alone (§10.10.11).
	import { statusIcons, statusInk } from '$lib/ui/status';

	let {
		text,
		failed = false,
		testid
	}: { text: string; failed?: boolean; testid: string } = $props();
	const kind = $derived(failed ? 'error' : 'success');
	const Icon = $derived(statusIcons[kind]);
</script>

<p role="status" class="flex items-center gap-2 text-small {statusInk[kind]}" data-testid={testid}>
	{#if text}<Icon aria-hidden="true" />{text}{/if}
</p>
