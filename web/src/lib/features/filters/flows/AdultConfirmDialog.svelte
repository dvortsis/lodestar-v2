<script lang="ts">
	// The 18+ confirmation for "Show adult content" (§20.9, B17). A preference, not a password:
	// it says what turning it on does and asks for confirmation. Focus starts on Cancel.
	import Button from '$lib/ui/controls/Button.svelte';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { AdultConfirm } from './adult.svelte';

	interface Props {
		confirm: AdultConfirm;
	}

	let { confirm }: Props = $props();

	let cancelEl = $state<HTMLElement | null>(null);
	let open = $derived(confirm.open);
</script>

<Dialog
	bind:open
	title={m.filters_flows_adult_title()}
	closeLabel={m.filters_flows_close()}
	onOpenChange={(next) => !next && confirm.answer(false)}
	initialFocus={() => cancelEl?.querySelector('button')}
>
	<div class="flex flex-col gap-3" data-testid="adult-confirm">
		<p>{m.filters_flows_adult_body()}</p>
		<p class="font-medium">{m.filters_flows_adult_age()}</p>
	</div>
	{#snippet footer()}
		<span bind:this={cancelEl} class="contents">
			<Button variant="quiet" onclick={() => confirm.answer(false)} data-testid="adult-cancel">
				{m.filters_flows_cancel()}
			</Button>
		</span>
		<Button variant="primary" onclick={() => confirm.answer(true)} data-testid="adult-show">
			{m.filters_flows_adult_confirm()}
		</Button>
	{/snippet}
</Dialog>
