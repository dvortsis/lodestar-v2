<script lang="ts">
	// The full-page "Restarting…" state (§10.10.6): polls /healthz with backoff, then reloads (or
	// goes to the new address), after checking that nothing still waits for a restart. After two
	// minutes it says what to check. Loaded only when a restart starts.
	import { onMount } from 'svelte';
	import { api } from '$lib/api/client';
	import { formatCount } from '$lib/format';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import { waitForServer } from './flow';

	import type { Waiting } from './logic';

	let { target, startedAt, estimateSeconds }: Waiting = $props();
	let phase = $state<'waiting' | 'timeout' | 'pending'>('waiting');
	let root = $state<HTMLElement>();
	let heading = $state<HTMLElement>();
	let ctl: AbortController | undefined;

	const sleep = (ms: number) => new Promise<void>((done) => setTimeout(done, ms));

	async function healthy(): Promise<boolean> {
		try {
			if (target) {
				// Another origin: an opaque answer is enough to know something is listening.
				await fetch(new URL('/healthz', target), { mode: 'no-cors', cache: 'no-store' });
				return true;
			}
			return (await fetch('/healthz', { cache: 'no-store' })).ok;
		} catch {
			return false;
		}
	}

	async function run() {
		ctl?.abort();
		const mine = (ctl = new AbortController());
		phase = 'waiting';
		const result = await waitForServer({
			probe: healthy,
			isNewRun:
				target || startedAt === null
					? undefined
					: async () => (await api.get('/system')).started_at !== startedAt,
			now: Date.now,
			sleep,
			settleMs: estimateSeconds * 1000,
			signal: mine.signal
		});
		if (result === 'aborted') return;
		if (result === 'timeout') {
			phase = 'timeout';
			return;
		}
		if (target) {
			// Signing in is per address; the keep-or-revert prompt waits in Settings there.
			location.assign(new URL('/settings', target).href);
			return;
		}
		const left = await api.get('/system').then(
			(s) => s.restart_required.length,
			() => 0
		);
		if (left > 0) phase = 'pending';
		else location.reload();
	}

	onMount(() => {
		// A real modal: out of the page's <main> (no nested landmark), with everything behind it inert
		// so Tab can't reach the Settings page that is going away.
		const behind = [...document.body.children].filter((el) => el !== root);
		document.body.append(root!);
		for (const el of behind) el.setAttribute('inert', '');
		heading?.focus();
		void run();
		return () => {
			ctl?.abort();
			for (const el of behind) el.removeAttribute('inert');
		};
	});
</script>

<div
	bind:this={root}
	role="dialog"
	aria-modal="true"
	aria-labelledby="restarting-title"
	class="fixed inset-0 z-50 flex items-center justify-center bg-base-100 p-6"
	data-testid="restarting-page"
	data-phase={phase}
>
	<div class="flex max-w-md flex-col gap-4">
		<h1 id="restarting-title" bind:this={heading} tabindex="-1" class="text-title font-semibold">
			{m.restart_page_title()}
		</h1>
		<p role="status" data-testid="restarting-status">
			{#if phase === 'waiting'}
				{m.restart_page_waiting({ seconds: formatCount(estimateSeconds) })}
			{:else if phase === 'pending'}
				{m.restart_page_pending()}
			{:else}
				{m.restart_page_timeout()}
			{/if}
		</p>
		{#if phase === 'timeout'}
			<ul class="flex flex-col gap-1" data-testid="restarting-checks">
				<li>{m.restart_page_check_logs()}</li>
				<li>{m.restart_page_check_healthcheck()}</li>
			</ul>
			<div><Button onclick={() => void run()}>{m.restart_page_try_again()}</Button></div>
		{:else if phase === 'pending'}
			<div><Button onclick={() => location.reload()}>{m.restart_page_reload()}</Button></div>
		{/if}
	</div>
</div>
