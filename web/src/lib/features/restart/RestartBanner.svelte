<script lang="ts">
	// "3 changes apply after a restart · Review · Restart now" on every Settings screen and the
	// Overview (§10.10.6). This is all a Settings first view carries; Review's list (it reads the
	// form engine), the confirmation dialog, the Restarting page and "Keep this address?" load when
	// they are needed.
	import { onMount, type Component } from 'svelte';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import type { Pending, System, Waiting } from './logic';
	import { restart } from './restart.svelte';

	// The count is a small whole number, so it goes in as is: `$lib/format` is 0.8 KB gzipped that a
	// Settings first view otherwise doesn't carry.
	const door = () => import('./door');
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
			void door()
				.then((d) => d.parts.prompt())
				.then((c) => (Prompt = c));
	});

	// The full-page state is the store's, so it outlives the dialog and the prompt that began it.
	$effect(() => {
		if (restart.waiting && !Restarting)
			void door()
				.then((d) => d.parts.restarting())
				.then((c) => (Restarting = c));
	});

	async function review() {
		reviewing = !reviewing;
		if (reviewing && !Review) Review = await (await door()).parts.review();
	}
	async function openDialog() {
		Dialog ??= await (await door()).parts.dialog();
		confirming = true;
	}
</script>

{#if awaiting && Prompt && restart.system}<Prompt system={restart.system} />{/if}

{#if pending}
	<div class="flex flex-col gap-3" data-testid="restart-banner">
		<!-- Plain markup on purpose: this is all of the feature a Settings first view carries. -->
		<div
			role="status"
			class="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-box border border-info bg-base-100 p-3"
		>
			<span class="min-w-0 flex-1">
				<span data-testid="restart-count"
					>{(pending.fields.length === 1 ? m.restart_banner_one : m.restart_banner)({
						count: pending.fields.length
					})}</span
				>
				{#if !pending.canRestart && pending.hint}
					<span data-testid="restart-hint"> · {pending.hint}</span>
				{/if}
			</span>
			<button
				type="button"
				class="btn btn-ghost {FOCUS_RING} {TARGET_MIN}"
				aria-expanded={reviewing}
				onclick={() => void review()}>{m.restart_review()}</button
			>
			{#if pending.canRestart}
				<button
					type="button"
					class="btn btn-primary {FOCUS_RING} {TARGET_MIN}"
					onclick={() => void openDialog()}>{m.restart_now()}</button
				>
			{/if}
		</div>
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
