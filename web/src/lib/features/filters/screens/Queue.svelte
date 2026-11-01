<script lang="ts">
	// The review queue (§20.7): what the filters are holding, by hash and rule. It never shows a
	// name: a sealed name opens only through the reveal dialog (warning and password). A hold marked
	// adult is left out unless "Show adult content" is on. Release, False positive and Confirm
	// each go through the flow's dialog, and the list reloads when the overview's version moves.
	import { onDestroy } from 'svelte';
	import { api, isAbort } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { confirmAbuse, falsePositive, release } from './ops';
	import type { Model, Queue } from './model.svelte';
	import type { Ask } from './types';

	let { model, ask }: { model: Model; ask: Ask } = $props();

	let queue = $state.raw<Queue | null>(null);
	let more = $state.raw<Queue['quarantine']>([]);
	let failed = $state(false);
	let revealHash = $state('');
	let revealOpen = $state(false);
	let Reveal = $state.raw<typeof import('../flows/RevealNameDialog.svelte').default>();
	let ctl: AbortController | undefined;

	async function load(cursor?: string) {
		ctl?.abort();
		const mine = (ctl = new AbortController());
		try {
			const q = await api.get('/filters/queue', {
				query: cursor ? { cursor } : {},
				signal: mine.signal
			});
			if (cursor) {
				more = [...more, ...q.quarantine];
				queue = {
					...q,
					quarantine: queue?.quarantine ?? [],
					decisions_30d: queue?.decisions_30d ?? []
				};
			} else {
				more = [];
				queue = q;
			}
			failed = false;
		} catch (e) {
			if (!isAbort(e)) failed = true;
		}
	}
	// Any new overview (a change applied, a reload) reads the queue again; a release doesn't bump the
	// rule-set version, so don't compare versions.
	$effect(() => {
		void model.overview;
		void load();
	});
	// A page of nothing but adult holds (hidden while 18+ is off) reads on to the next one, so the
	// screen never says "nothing" or offers "more" for what it can't show.
	$effect(() => {
		if (queue?.next_cursor && rows.length === 0) void load(queue.next_cursor);
	});
	onDestroy(() => ctl?.abort());

	const rows = $derived(
		queue ? [...queue.quarantine, ...more].filter((r) => model.adult || !r.adult_hold) : []
	);
	async function reveal(hash: string) {
		Reveal ??= (await (await import('../flows-door')).load()).RevealNameDialog;
		revealHash = hash;
		revealOpen = true;
	}
</script>

<section class="flex flex-col gap-3" aria-labelledby="filters-queue-h" data-testid="filters-queue">
	<h2 id="filters-queue-h" class="text-label font-semibold">{m.filters_queue_heading()}</h2>
	<p class="text-muted-text">{m.filters_queue_help()}</p>
	{#if failed}
		<Banner kind="error">
			{m.filters_queue_failed()}
			{#snippet action()}
				<Button onclick={() => load()}>{m.filters_retry()}</Button>
			{/snippet}
		</Banner>
	{:else if queue === null}
		<p role="status" class="sr-only">{m.filters_loading()}</p>
	{:else if rows.length === 0 && !queue.next_cursor}
		<p class="text-muted-text" data-testid="queue-empty">{m.filters_queue_empty()}</p>
	{/if}
	<ul class="flex flex-col gap-2">
		{#each rows as r (r.hash + r.rule)}
			<li class="flex flex-wrap items-center gap-3" data-testid="queue-{r.hash}">
				<Mono wrap>{r.hash}</Mono>
				<Badge>{r.rule}</Badge>
				{#if r.sealed}
					<Button dense onclick={() => reveal(r.hash)} data-testid="queue-reveal-{r.hash}"
						>{m.filters_queue_reveal()}</Button
					>
				{/if}
				<Button
					dense
					onclick={() =>
						ask(release(r.hash), {
							title: m.filters_queue_release_title(),
							applyLabel: m.filters_queue_release()
						})}
					data-testid="queue-release-{r.hash}">{m.filters_queue_release()}</Button
				>
				<Button
					dense
					onclick={() =>
						ask(falsePositive(r.hash), {
							title: m.filters_queue_fp_title(),
							applyLabel: m.filters_queue_fp()
						})}
					data-testid="queue-fp-{r.hash}">{m.filters_queue_fp()}</Button
				>
				<Button
					dense
					onclick={() =>
						ask(confirmAbuse(r.hash), {
							title: m.filters_queue_confirm_title(),
							applyLabel: m.filters_queue_confirm()
						})}
					data-testid="queue-confirm-{r.hash}">{m.filters_queue_confirm()}</Button
				>
			</li>
		{/each}
	</ul>
	{#if queue?.next_cursor && rows.length > 0}
		<div>
			<Button onclick={() => load(queue?.next_cursor ?? undefined)} data-testid="queue-more"
				>{m.filters_queue_more()}</Button
			>
		</div>
	{/if}
	<!-- Per-rule counts include the adult-only rules, so they show only with 18+ on. -->
	{#if queue && model.adult && queue.decisions_30d.length}
		<h3 class="text-label font-semibold">{m.filters_decisions_heading()}</h3>
		<ul class="flex flex-col gap-1" data-testid="filters-decisions">
			{#each queue.decisions_30d as d (d.rule + d.action + d.shadow)}
				<li class="flex flex-wrap items-center gap-2">
					<Badge>{d.rule}</Badge>
					<span data-testid="decision-count"
						>{m.filters_decisions_count({ count: formatCount(d.count, currentLocale()) })}</span
					>
					{#if d.shadow}<Badge variant="info">{m.filters_rule_shadow()}</Badge>{/if}
				</li>
			{/each}
		</ul>
	{/if}
</section>

{#if Reveal}
	<Reveal hash={revealHash} bind:open={revealOpen} />
{/if}
