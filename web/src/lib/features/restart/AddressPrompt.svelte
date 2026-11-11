<script lang="ts">
	// "Keep this address?" (§10.10.6 lockout protection): the server is serving on the new HTTP
	// address and reverts by itself at `deadline` unless it hears from here. Keep confirms; Go back
	// reverts, and when the server restarts onto the old address the UI follows it there. Loaded
	// only while a change awaits confirmation.
	import { onMount, type Component } from 'svelte';
	import { api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { addressPromptOf, failureOf, type RestartFailure, type System } from './logic';
	import { restart } from './restart.svelte';

	let { system }: { system: System } = $props();
	let now = $state(Date.now());
	let busy = $state(false);
	let failure = $state<RestartFailure>();
	let Restarting = $state<
		Component<{
			target: string | null;
			startedAt: number | null;
			estimateSeconds: number;
		}>
	>();
	let wait = $state<{ target: string; estimateSeconds: number }>();
	const prompt = $derived(addressPromptOf(system, now));

	onMount(() => {
		const tick = setInterval(() => (now = Date.now()), 1_000);
		return () => clearInterval(tick);
	});

	async function answer(kind: 'keep' | 'revert') {
		if (!prompt) return;
		busy = true;
		failure = undefined;
		try {
			const outcome = await api.post(
				kind === 'keep' ? '/system/address/keep' : '/system/address/revert'
			);
			if (outcome.restarting) {
				Restarting = (await import('./RestartingPage.svelte')).default;
				// Going back changes the address again: the UI returns to where it was.
				wait = { target: prompt.previousUrl, estimateSeconds: system.restart.estimated_seconds };
			} else await restart.refresh();
		} catch (e) {
			failure = failureOf(e);
			await restart.refresh();
		} finally {
			busy = false;
		}
	}

	const mins = (s: number) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
</script>

{#if prompt}
	<Banner kind="warning" title={m.restart_keep_title()}>
		<p>{m.restart_keep_body({ url: prompt.url })}</p>
		<p data-testid="keep-countdown">
			{m.restart_keep_countdown({ time: mins(prompt.secondsLeft) })}
		</p>
		{#if failure}<p role="alert" data-testid="keep-failure">{failure.message}</p>{/if}
		{#snippet action()}
			<div class="flex flex-wrap gap-2">
				<Button variant="primary" loading={busy} onclick={() => void answer('keep')}
					>{m.restart_keep_yes()}</Button
				>
				<Button loading={busy} onclick={() => void answer('revert')}>{m.restart_keep_back()}</Button
				>
			</div>
		{/snippet}
	</Banner>
{/if}

{#if Restarting && wait}<Restarting {...wait} startedAt={null} />{/if}
