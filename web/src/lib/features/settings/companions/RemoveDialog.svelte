<script lang="ts">
	// What removing deletes, listed by the server, then Remove. Cancel and Esc send nothing.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import type { RemovalPlan } from './logic';

	interface Props {
		name: string;
		plan: RemovalPlan;
		busy: boolean;
		error: string;
		onconfirm: () => void;
		onclose: () => void;
	}
	let { name, plan, busy, error, onconfirm, onclose }: Props = $props();
	let open = $state(true);
</script>

<Dialog
	bind:open
	title={m.settings_companions_remove_title({ name })}
	closeLabel={m.settings_companions_close()}
	onOpenChange={(o) => {
		if (!o) onclose();
	}}
	initialFocus={() => document.querySelector<HTMLElement>('[data-remove-cancel]')}
>
	<div class="flex flex-col gap-4">
		<p class="text-small">{m.settings_companions_remove_intro({ name })}</p>
		<ul class="list-disc ps-4 text-small">
			{#each plan.deletes as d (d.kind + d.name)}
				<li class="break-all font-mono text-caption">
					{d.kind === 'folder'
						? m.settings_companions_remove_folder({ name: d.name })
						: m.settings_companions_remove_setting({ name: d.name })}
				</li>
			{/each}
		</ul>
		{#if error}<Banner kind="error">{error}</Banner>{/if}
	</div>
	{#snippet footer()}
		<Button data-remove-cancel onclick={() => (open = false)}
			>{m.settings_companions_cancel()}</Button
		>
		<Button variant="danger" loading={busy} onclick={onconfirm}
			>{m.settings_companions_remove()}</Button
		>
	{/snippet}
</Dialog>
