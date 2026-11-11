<script lang="ts">
	// "3 changes apply after a restart · Review · Restart now" on every Settings screen and the
	// Overview (§10.10.6). This is all a Settings first view carries; Review's list (it reads the
	// form engine), the confirmation dialog, the Restarting page and "Keep this address?" load when
	// they are needed.
	import { onMount, type Component } from 'svelte';
	import { page } from '$app/state';
	import { formatCount } from '$lib/format';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import type { Pending, System, Waiting } from './logic';
	import { restart } from './restart.svelte';

	const REFRESH_MS = 15_000;
	let reviewing = $state(false);
	let Review = $state<Component<{ fields: string[] }>>();
	let Dialog =
		$state<Component<{ pending: Pending; startedAt: number | null; onclose: () => void }>>();
	let Prompt = $state<Component<{ system: System }>>();
	let Restarting = $state<Component<Waiting>>();
	let confirming = $state(false);

	const pending = $derived(restart.pending);
	const awaiting = $derived(restart.system?.address_change?.state === 'awaiting_confirmation');

	// Where `/system` was last asked: it is asked on a Settings screen's first view, on focus, on
	// each navigation inside Settings (a save there may have added a field) and every REFRESH_MS.
	let asked = 0;
	function refresh() {
		asked = Date.now();
		void restart.refresh();
	}
	onMount(() => {
		const every = setInterval(() => {
			if (!document.hidden && Date.now() - asked >= REFRESH_MS) refresh();
		}, 1_000);
		window.addEventListener('focus', refresh);
		return () => {
			clearInterval(every);
			window.removeEventListener('focus', refresh);
		};
	});
	$effect(() => {
		void page.url.pathname;
		refresh();
	});
	$effect(() => {
		if (awaiting && !Prompt)
			void import('./AddressPrompt.svelte').then((mod) => (Prompt = mod.default));
	});

	// The full-page state is the store's, so it outlives the dialog and the prompt that began it.
	$effect(() => {
		if (restart.waiting && !Restarting)
			void import('./RestartingPage.svelte').then((mod) => (Restarting = mod.default));
	});

	async function review() {
		reviewing = !reviewing;
		if (reviewing && !Review) Review = (await import('./ReviewList.svelte')).default;
	}
	async function openDialog() {
		Dialog ??= (await import('./RestartDialog.svelte')).default;
		confirming = true;
	}
</script>

{#if awaiting && Prompt && restart.system}<Prompt system={restart.system} />{/if}

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

{#if Restarting && restart.waiting}<Restarting {...restart.waiting} />{/if}
