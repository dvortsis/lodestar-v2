<script lang="ts">
	// The 18+ confirmation for adult torrents in exports (§20.9): the same question "Show adult
	// content" asks (B17), worded for exports. Focus starts on Cancel (§10.10.7).
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';

	let { open = $bindable(false), onanswer }: { open?: boolean; onanswer: (ok: boolean) => void } =
		$props();
	let cancelEl = $state<HTMLElement | null>(null);
</script>

<Dialog
	bind:open
	title={m.settings_outputs_adult_title()}
	closeLabel={m.settings_outputs_dialog_close()}
	onOpenChange={(next) => !next && onanswer(false)}
	initialFocus={() => cancelEl?.querySelector('button')}
>
	<div class="flex flex-col gap-3" data-testid="outputs-adult-confirm">
		<p>{m.settings_outputs_adult_body()}</p>
		<p class="font-medium">{m.settings_outputs_adult_age()}</p>
	</div>
	{#snippet footer()}
		<span bind:this={cancelEl} class="contents">
			<Button variant="quiet" onclick={() => onanswer(false)} data-testid="outputs-adult-cancel">
				{m.settings_outputs_adult_cancel()}
			</Button>
		</span>
		<Button variant="primary" onclick={() => onanswer(true)} data-testid="outputs-adult-include">
			{m.settings_outputs_adult_confirm()}
		</Button>
	{/snippet}
</Dialog>
