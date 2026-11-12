<script lang="ts">
	// The full-page "Restarting…" state (§10.10.6): polls /healthz with backoff, then reloads (or
	// goes to the new address), after checking that nothing still waits for a restart. After two
	// minutes it says what to check. Loaded only when a restart starts.
	import { onMount } from 'svelte';
	import { api } from '$lib/api/client';
	import { formatCount } from '$lib/format';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import { isOtherOrigin, waitForServer, waitUntilDown } from './flow';

	import type { Waiting } from './logic';

	let { target, startedAt, estimateSeconds }: Waiting = $props();
	let phase = $state<'waiting' | 'moving' | 'timeout' | 'pending'>('waiting');
	let root = $state<HTMLElement>();
	let heading = $state<HTMLElement>();
	let ctl: AbortController | undefined;

	// A different address is a different origin, and the page's policy (`connect-src 'self'`) lets it
	// fetch only itself. So it never probes the new address: it waits for this one to stop answering,
	// gives the server the time it estimates, then goes there once; the link is always shown.
	const other = $derived(isOtherOrigin(target, location.origin));
	const address = $derived(target ? new URL(target).host : '');
	const href = $derived(target ? new URL('/settings', target).href : '');

	const sleep = (ms: number) => new Promise<void>((done) => setTimeout(done, ms));

	async function healthy(): Promise<boolean> {
		try {
			return (await fetch('/healthz', { cache: 'no-store' })).ok;
		} catch {
			return false;
		}
	}

	async function run() {
		ctl?.abort();
		const mine = (ctl = new AbortController());
		if (other) {
			phase = 'moving';
			const common = { probe: healthy, now: Date.now, sleep, signal: mine.signal };
			const down = await waitUntilDown(common);
			if (down === 'aborted') return;
			if (down === 'down') {
				await sleep(estimateSeconds * 1000 + 2_000);
				if (!mine.signal.aborted) location.assign(href);
				return;
			}
			phase = 'timeout';
			return;
		}
		phase = 'waiting';
		const result = await waitForServer({
			probe: healthy,
			isNewRun:
				startedAt === null
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
			{:else if phase === 'moving'}
				{m.restart_page_moving({ address })}
			{:else if phase === 'pending'}
				{m.restart_page_pending()}
			{:else}
				{m.restart_page_timeout()}
			{/if}
		</p>
		{#if other}
			<div>
				<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- the server's own other address -->
				<a class="btn btn-primary {FOCUS_RING} {TARGET_MIN}" {href} data-testid="restarting-open"
					>{m.restart_page_open({ address })}</a
				>
			</div>
		{/if}
		{#if phase === 'timeout'}
			<ul class="flex flex-col gap-1" data-testid="restarting-checks">
				<li>{m.restart_page_check_logs()}</li>
				<li>{m.restart_page_check_healthcheck()}</li>
			</ul>
			<div>
				<Button variant={other ? 'secondary' : 'primary'} onclick={() => void run()}
					>{m.restart_page_try_again()}</Button
				>
			</div>
		{:else if phase === 'pending'}
			<div><Button onclick={() => location.reload()}>{m.restart_page_reload()}</Button></div>
		{/if}
	</div>
</div>
