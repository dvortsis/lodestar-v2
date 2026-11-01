<script lang="ts">
	// Settings → Filters & Safety (§20.7): switches, your rules, exceptions and the review queue.
	// Every change goes through the flows' dialog (preview, warning, password). The screen only
	// shows what the server last said; it decides nothing about a change.
	import { onDestroy, onMount, type Component } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import { undoChange } from './ops';
	import { createModel } from './model.svelte';
	import Switches from './Switches.svelte';
	import Rules from './Rules.svelte';

	const model = createModel();
	let Dialog = $state<Component<Record<string, unknown>>>();
	let dialogFailed = $state(false);

	// The dialog loads on the first change, not with the screen.
	async function ensureDialog() {
		if (Dialog) return true;
		try {
			Dialog = (await import('../flows/ChangeDialog.svelte')).default as never;
			return true;
		} catch {
			dialogFailed = true;
			return false;
		}
	}
	const ask: typeof model.start = async (change, p) => {
		dialogFailed = false;
		try {
			if (await ensureDialog()) await model.start(change, p);
		} catch {
			dialogFailed = true;
		}
	};

	let heading = $state<HTMLElement>();
	// A change can remove the row whose button opened the dialog; focus then falls to the page's top
	// rather than to <body> (§10.13).
	function keepFocus() {
		requestAnimationFrame(() =>
			requestAnimationFrame(() => {
				if (!document.activeElement || document.activeElement === document.body) heading?.focus();
			})
		);
	}

	onMount(model.load);
	onDestroy(model.destroy);
</script>

<h1 class="text-title font-semibold" tabindex="-1" bind:this={heading}>
	{m.filters_title()}
</h1>
<p class="text-muted-text">{m.filters_purpose()}</p>

{#if dialogFailed}
	<div data-testid="filters-dialog-failed">
		<Banner kind="error">{m.filters_dialog_failed()}</Banner>
	</div>
{/if}

{#if model.overview && model.failed}
	<div data-testid="filters-stale">
		<Banner kind="warning">
			{m.filters_stale()}
			{#snippet action()}
				<Button onclick={model.load} data-testid="filters-reload">{m.filters_retry()}</Button>
			{/snippet}
		</Banner>
	</div>
{/if}

{#if model.overview}
	{@const o = model.overview}
	<div class="flex flex-col gap-8" data-testid="filters-screen">
		<Switches {o} {ask} />
		<Rules {o} {ask} reload={model.load} />
		<section class="flex flex-col gap-3" aria-labelledby="filters-undo-h">
			<h2 id="filters-undo-h" class="text-label font-semibold">{m.filters_undo_heading()}</h2>
			<p class="text-muted-text">{m.filters_undo_help()}</p>
			<div>
				<Button
					onclick={() => ask(undoChange(), { title: m.filters_undo_title() })}
					data-testid="filters-undo">{m.filters_undo()}</Button
				>
			</div>
		</section>
		{#await import('./Queue.svelte') then { default: Queue }}
			<Queue {model} {ask} />
		{:catch}
			<Banner kind="error">{m.filters_part_failed()}</Banner>
		{/await}
	</div>
{:else if model.failed}
	<div data-testid="filters-failed">
		<Banner kind="error">
			{m.filters_load_failed()}
			{#snippet action()}
				<Button onclick={model.load} data-testid="filters-retry">{m.filters_retry()}</Button>
			{/snippet}
		</Banner>
	</div>
{:else}
	<p role="status" class="sr-only">{m.filters_loading()}</p>
	<Skeleton lines={6} />
{/if}

{#if Dialog && model.flow}
	<Dialog
		flow={model.flow}
		title={model.pending.title}
		applyLabel={model.pending.applyLabel}
		shadowDays={model.pending.shadowDays}
		onapplied={model.load}
		onclose={keepFocus}
	/>
{/if}
