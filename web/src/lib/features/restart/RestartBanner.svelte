<script lang="ts">
	// "3 changes apply after a restart · Review · Restart now" on every Settings screen and the
	// Overview (§10.10.6), and "Keep this address?" wherever the UI is while a new address waits.
	// It floats at the bottom of the viewport, so it never moves the screen under it. It is mounted by
	// `watch.svelte.ts` and is announced by that file's live region; Review's list (it reads the form
	// engine), the dialog and the Restarting page load when they are needed.
	import { onMount, type Component } from 'svelte';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import type { Pending, System, Waiting } from './logic';
	import { restart } from './restart.svelte';
	import { countText } from './text';

	const door = () => import('./door');
	let reviewing = $state(false);
	let Review = $state<Component<{ fields: string[] }>>();
	let Dialog =
		$state<Component<{ pending: Pending; startedAt: number | null; onclose: () => void }>>();
	let Prompt = $state<Component<{ system: System }>>();
	let Restarting = $state<Component<Waiting>>();
	let confirming = $state(false);

	const onSettings = $derived(page.url.pathname.startsWith('/settings'));
	const pending = $derived(onSettings ? restart.pending : null);
	const awaiting = $derived(restart.system?.address_change?.state === 'awaiting_confirmation');

	onMount(() => void restart.refresh());

	// The tray floats above the page, so the page makes room for it: while it shows, the body gets
	// bottom padding (and scroll padding) of the tray's height plus its lift, and the tray sits above
	// the Settings save bar at that bar's measured height, however tall it wraps. Both follow resizes.
	const MIN_LIFT = 72; // the first toast; the save bar is 56 px and gets a gap of 16 px
	let tray = $state<HTMLElement>();
	let lift = $state(MIN_LIFT);
	$effect(() => {
		if (!tray) return;
		const el = tray;
		const measure = () => {
			const bar = document.querySelector<HTMLElement>('[data-testid="save-bar"]');
			const next = Math.max(MIN_LIFT, bar ? bar.offsetHeight + 16 : 0);
			lift = next;
			const room = `${next + el.offsetHeight + 16}px`;
			document.body.style.paddingBottom = room;
			document.documentElement.style.scrollPaddingBottom = room;
		};
		let frame = 0;
		const soon = () => {
			cancelAnimationFrame(frame);
			frame = requestAnimationFrame(measure);
		};
		measure();
		// (No observers where the browser has none, as in a unit test's DOM.)
		const sizes = typeof ResizeObserver === 'undefined' ? undefined : new ResizeObserver(soon);
		sizes?.observe(el);
		// The save bar comes and goes with the screen and its edits.
		const watch = new MutationObserver(soon);
		watch.observe(document.querySelector('main') ?? document.body, {
			childList: true,
			subtree: true
		});
		return () => {
			cancelAnimationFrame(frame);
			sizes?.disconnect();
			watch.disconnect();
			document.body.style.paddingBottom = '';
			document.documentElement.style.scrollPaddingBottom = '';
		};
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

{#if (pending || (awaiting && Prompt)) && !restart.waiting}
	<div
		bind:this={tray}
		class="restart-tray"
		style:bottom="{lift}px"
		role="region"
		aria-label={m.restart_tray_label()}
		data-testid="restart-tray"
	>
		{#if awaiting && Prompt && restart.system}<Prompt system={restart.system} />{/if}
		{#if pending}
			<div class="flex flex-col gap-3" data-testid="restart-banner">
				<div
					class="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-box border border-info bg-base-100 p-3"
				>
					<span class="min-w-0 flex-1">
						<span data-testid="restart-count">{countText(pending)}</span>
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
				{#if reviewing && Review}<div class="restart-review">
						<Review fields={pending.fields} />
					</div>{/if}
			</div>
		{/if}
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

<style>
	.restart-tray {
		position: fixed;
		inset-inline: calc(var(--spacing) * 4);
		/* Above the first toast and the Settings save bar; the script sets the real lift. */
		bottom: calc(var(--spacing) * 14 + var(--spacing) * 4);
		z-index: 40;
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 3);
	}
	.restart-review {
		max-height: 40vh;
		overflow-y: auto;
		border-radius: var(--radius-box);
		background: var(--color-base-100);
		padding: calc(var(--spacing) * 3);
	}
	@media (min-width: 640px) {
		.restart-tray {
			inset-inline-start: auto;
			max-width: 32rem;
		}
	}
</style>
