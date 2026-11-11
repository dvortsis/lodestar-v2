<script lang="ts">
	// The confirmation dialog for Restart now (§10.10.6): the pause the server estimates and, for an
	// HTTP address change, the exact new URL. A 409 shows its message and stops; a 202 hands over to
	// the full-page Restarting state. Loaded when Restart now is pressed.
	import type { Component } from 'svelte';
	import { api } from '$lib/api/client';
	import { formatCount } from '$lib/format';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import { failureOf, type Pending, type RestartFailure } from './logic';

	interface Props {
		pending: Pending;
		/** The run that is ending (`GET /system` `started_at`). */
		startedAt: number | null;
		onclose: () => void;
	}
	let { pending, startedAt, onclose }: Props = $props();
	let open = $state(true);
	let busy = $state(false);
	let failure = $state<RestartFailure>();
	let Restarting = $state<
		Component<{
			target: string | null;
			startedAt: number | null;
			estimateSeconds: number;
		}>
	>();
	let wait = $state<{ estimateSeconds: number }>();

	async function restartNow() {
		busy = true;
		failure = undefined;
		try {
			const accepted = await api.post('/system/restart');
			Restarting = (await import('./RestartingPage.svelte')).default;
			wait = { estimateSeconds: accepted.estimated_seconds };
			open = false;
		} catch (e) {
			failure = failureOf(e);
		} finally {
			busy = false;
		}
	}
</script>

<Dialog
	bind:open
	title={m.restart_confirm_title()}
	closeLabel={m.restart_close()}
	onOpenChange={(o) => {
		if (!o && !wait) onclose();
	}}
	initialFocus={() => document.querySelector<HTMLElement>('[data-restart-cancel]')}
>
	<p data-testid="restart-warning">
		{m.restart_confirm_body({ seconds: formatCount(pending.estimateSeconds) })}
	</p>
	{#if pending.newUrl}
		<p class="mt-2">
			{m.restart_confirm_address()}
			<span class="font-mono break-words" data-testid="restart-new-url">{pending.newUrl}</span>
		</p>
		<p class="mt-2 text-muted-text">{m.restart_confirm_address_help()}</p>
	{/if}
	{#if failure}<p class="mt-2" role="alert" data-testid="restart-failure">{failure.message}</p>{/if}
	{#snippet footer()}
		<Button data-restart-cancel onclick={() => (open = false)}>{m.restart_cancel()}</Button>
		{#if failure?.kind !== 'unavailable'}
			<Button variant="primary" loading={busy} onclick={() => void restartNow()}
				>{m.restart_now()}</Button
			>
		{/if}
	{/snippet}
</Dialog>

{#if Restarting && wait}
	<Restarting target={pending.newUrl} {startedAt} estimateSeconds={wait.estimateSeconds} />
{/if}
