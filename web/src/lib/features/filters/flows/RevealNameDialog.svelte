<script lang="ts">
	// Reveal one sealed name (§20.5.5, §20.11): a weakening change, so the server's warning and the
	// password. The name is shown here only, in Mono, as text. It lives in the flow's `done` state
	// and goes with it when the dialog closes; there is no Copy button, and it never reaches the
	// URL, the title, a toast, storage or the console.
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import ChangeDialog from './ChangeDialog.svelte';
	import { revealName } from './changes';
	import { createChangeFlow } from './flow.svelte';

	interface Props {
		hash: string;
		open: boolean;
		onclose?: () => void;
	}

	let { hash, open = $bindable(), onclose }: Props = $props();

	const flow = createChangeFlow();

	// Opening previews the reveal (warning + password); the dialog itself then owns the flow.
	let started = false;
	$effect(() => {
		if (open && !started) {
			started = true;
			flow.start(revealName(hash));
		} else if (!open) started = false;
	});
</script>

<ChangeDialog
	{flow}
	bind:open
	title={m.filters_flows_reveal_title()}
	applyLabel={m.filters_flows_reveal_apply()}
	{onclose}
>
	{#snippet result(response)}
		<div class="flex flex-col gap-3">
			<p class="text-muted-text">{m.filters_flows_reveal_lead()}</p>
			<div class="flex flex-col gap-1">
				<span class="text-small font-medium">{m.filters_flows_reveal_name()}</span>
				<span data-testid="reveal-name"><Mono wrap>{response.name ?? '—'}</Mono></span>
			</div>
		</div>
	{/snippet}
</ChangeDialog>
