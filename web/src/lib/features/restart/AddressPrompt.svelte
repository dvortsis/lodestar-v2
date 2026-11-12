<script lang="ts">
	// "Keep this address?" (§10.10.6 lockout protection): the server is serving on the new HTTP
	// address and reverts by itself at `deadline` unless it hears from here. Keep confirms; Go back
	// reverts, and when the server restarts onto the old address the UI follows it there. Loaded
	// only while a change awaits confirmation.
	import { onMount } from 'svelte';
	import { api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import { addressPromptOf, failureOf, type RestartFailure } from './flow';
	import type { System } from './logic';
	import { restart } from './restart.svelte';

	let { system }: { system: System } = $props();
	let now = $state(Date.now());
	let busy = $state(false);
	let failure = $state<RestartFailure>();
	const prompt = $derived(addressPromptOf(system, now, location.hostname));

	onMount(() => {
		const tick = setInterval(() => (now = Date.now()), 1_000);
		return () => clearInterval(tick);
	});

	// Going back changes the address again: the UI returns to where it was.
	function goBack(url: string) {
		restart.begin({
			target: url,
			startedAt: null,
			estimateSeconds: system.restart.estimated_seconds
		});
	}
	// The countdown runs on this browser's clock against the server's deadline, and the two can
	// disagree. So at 0:00 the page asks the server, and goes back only when the server says the
	// address was reverted (and can restart itself), or has stopped answering for FAILURES checks in
	// a row (it is restarting onto the old address). `address_change: null` means it was kept: the
	// prompt just goes. While the server still waits, Keep stays and the page asks again every 5 s.
	const FAILURES = 3;
	let nextCheck = 0;
	let checking = false;
	let failed = 0;
	let answered = false;
	$effect(() => {
		if (!prompt || prompt.secondsLeft > 0 || busy || checking || now < nextCheck) return;
		checking = true;
		const back = prompt.previousUrl;
		void restart.refresh().then((ok) => {
			checking = false;
			nextCheck = Date.now() + 5_000;
			// Keep or Go back was pressed while this was in flight: that answer is the newer one.
			if (answered) return;
			failed = ok ? 0 : failed + 1;
			const state = restart.system?.address_change?.state;
			if (ok && state === 'reverted' && restart.system?.restart.available) goBack(back);
			else if (failed >= FAILURES) goBack(back);
		});
	});

	async function answer(kind: 'keep' | 'revert') {
		if (!prompt) return;
		busy = true;
		failure = undefined;
		try {
			const outcome = await api.post(
				kind === 'keep' ? '/system/address/keep' : '/system/address/revert'
			);
			answered = true;
			if (outcome.restarting) {
				goBack(prompt.previousUrl);
			} else restart.setAddressChange(outcome.address_change);
		} catch (e) {
			failure = failureOf(e);
			await restart.refresh();
		} finally {
			busy = false;
		}
	}

	const mins = (s: number) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
</script>

<!-- Not a live region: the countdown ticks every second. The prompt is announced once, by the
restart watch's region. -->
{#if prompt}
	<section
		class="flex flex-col gap-2 rounded-box border border-warning bg-base-100 p-3"
		aria-labelledby="keep-title"
	>
		<h2 id="keep-title" class="font-medium">{m.restart_keep_title()}</h2>
		<p>{m.restart_keep_body({ url: prompt.url })}</p>
		<p data-testid="keep-countdown">
			{m.restart_keep_countdown({ time: mins(prompt.secondsLeft) })}
		</p>
		{#if failure}<p data-testid="keep-failure">{failure.message}</p>{/if}
		<div class="flex flex-wrap gap-2">
			<button
				type="button"
				class="btn btn-primary {FOCUS_RING} {TARGET_MIN}"
				aria-busy={busy || undefined}
				onclick={() => void (busy || answer('keep'))}>{m.restart_keep_yes()}</button
			>
			<button
				type="button"
				class="btn btn-outline border-field-border {FOCUS_RING} {TARGET_MIN}"
				aria-busy={busy || undefined}
				onclick={() => void (busy || answer('revert'))}>{m.restart_keep_back()}</button
			>
		</div>
	</section>
{/if}
