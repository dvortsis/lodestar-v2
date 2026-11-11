<script lang="ts">
	// "3 changes apply after a restart · Review · Restart now" on every Settings screen and the
	// Overview (§10.10.6), and the "Keep this address?" prompt that follows an HTTP address change.
	// Everything heavier (Review's form engine, the dialog, the Restarting page) loads when used, so
	// the Settings routes stay within budget.
	import { onMount, type Component } from 'svelte';
	import { page } from '$app/state';
	import { api } from '$lib/api/client';
	import { formatCount } from '$lib/format';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { addressPromptOf, failureOf, type Pending, type RestartFailure } from './logic';
	import { restart } from './restart.svelte';

	const REFRESH_MS = 15_000;
	let busy = $state(false);
	let failure = $state<RestartFailure>();
	let reviewing = $state(false);
	let Review = $state<Component<{ fields: string[] }>>();
	let Dialog =
		$state<Component<{ pending: Pending; startedAt: number | null; onclose: () => void }>>();
	let Restarting = $state<
		Component<{
			target: string | null;
			startedAt: number | null;
			estimateSeconds: number;
		}>
	>();
	let wait = $state<{ target: string | null; estimateSeconds: number }>();
	let confirming = $state(false);
	let now = $state(Date.now());
	let lastRefresh = Date.now();

	const pending = $derived(restart.pending);
	const prompt = $derived(addressPromptOf(restart.system, now));

	function refresh() {
		lastRefresh = Date.now();
		void restart.refresh();
	}

	onMount(() => {
		refresh();
		// One tick a second drives the countdown; the server is asked only every REFRESH_MS.
		const every = setInterval(() => {
			now = Date.now();
			if (!document.hidden && now - lastRefresh >= REFRESH_MS) refresh();
		}, 1_000);
		window.addEventListener('focus', refresh);
		return () => {
			clearInterval(every);
			window.removeEventListener('focus', refresh);
		};
	});
	// Another Settings screen: a save there may have added a field.
	$effect(() => {
		void page.url.pathname;
		refresh();
	});

	async function review() {
		reviewing = !reviewing;
		if (reviewing && !Review) Review = (await import('./ReviewList.svelte')).default;
	}

	async function openDialog() {
		Dialog ??= (await import('./RestartDialog.svelte')).default;
		confirming = true;
	}

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
				wait = {
					target: prompt.previousUrl,
					estimateSeconds: restart.system?.restart.estimated_seconds ?? 0
				};
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

{#if pending}
	<div class="flex flex-col gap-3" data-testid="restart-banner">
		<Banner kind="info">
			<span data-testid="restart-count"
				>{(pending.fields.length === 1 ? m.restart_banner_one : m.restart_banner)({
					count: formatCount(pending.fields.length)
				})}</span
			>
			{#if !pending.canRestart && pending.hint}
				<span data-testid="restart-hint"> · {pending.hint}</span>
			{/if}
			{#snippet action()}
				<div class="flex flex-wrap gap-2">
					<Button variant="quiet" aria-expanded={reviewing} onclick={() => void review()}
						>{m.restart_review()}</Button
					>
					{#if pending.canRestart}
						<Button variant="primary" onclick={() => void openDialog()}>{m.restart_now()}</Button>
					{/if}
				</div>
			{/snippet}
		</Banner>
		{#if reviewing && Review}<Review fields={pending.fields} />{/if}
	</div>
{/if}

{#if confirming && Dialog && pending}
	<Dialog
		{pending}
		startedAt={restart.system?.started_at ?? null}
		onclose={() => (confirming = false)}
	/>
{/if}

{#if Restarting && wait}<Restarting {...wait} startedAt={null} />{/if}
