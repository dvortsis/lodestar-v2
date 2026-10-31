<script lang="ts">
	// "Natural phrasing" (§10.4): the server reads the bar's text as plain words ("4k movies from
	// 2020 over 10gb") and proposes the clauses it means. They are shown as chips, and nothing
	// changes until the user applies them.
	import { onMount, tick } from 'svelte';
	import { api, ApiError, isAbort } from '$lib/api/client';
	import type { components } from '$lib/api/schema';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Chip from '$lib/ui/labels/Chip.svelte';

	interface Props {
		/** The bar's text. */
		text: string;
		/** The proposed query, to put in the bar and search. */
		apply: (canonical: string) => void;
		dismiss: () => void;
		announce: (text: string) => void;
	}
	let { text, apply, dismiss, announce }: Props = $props();

	type Proposal = components['schemas']['NaturalProposal'];
	let proposal = $state.raw<Proposal | null>(null);
	let status = $state<'loading' | 'ready' | 'none' | 'error'>('loading');
	let reason = $state('');
	let actions = $state<HTMLElement>();

	onMount(() => {
		const ctl = new AbortController();
		void (async () => {
			try {
				const r = await api.get('/parse', {
					query: { q: text, natural: true },
					signal: ctl.signal
				});
				if (r.natural?.chips.length) {
					proposal = r.natural;
					status = 'ready';
					announce(m.search_assist_natural_ready({ count: r.natural.chips.length }));
					await tick();
					actions?.querySelector('button')?.focus(); // Apply, the first
				} else {
					status = 'none';
					announce(m.search_assist_natural_none());
				}
			} catch (e) {
				if (isAbort(e)) return;
				status = 'error';
				reason = e instanceof ApiError && e.code.startsWith('natural_') ? e.message : '';
				announce(m.search_assist_natural_failed());
			}
		})();
		return () => ctl.abort();
	});

	function onkeydown(e: KeyboardEvent) {
		if (e.key !== 'Escape') return;
		e.preventDefault();
		dismiss();
	}
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section class="assist-panel" aria-label={m.search_assist_natural_title()} {onkeydown}>
	{#if status === 'loading'}
		<p role="status" class="assist-note">{m.search_assist_natural_loading()}</p>
	{:else if status === 'ready' && proposal}
		<ul class="assist-chips" aria-label={m.search_assist_natural_chips()}>
			{#each proposal.chips as chip (chip.text + chip.from.start)}
				<li><Chip><span class="font-mono">{chip.text}</span></Chip></li>
			{/each}
		</ul>
		{#if proposal.text.length}
			<p class="assist-note">
				{m.search_assist_natural_text({ words: proposal.text.join(' ') })}
			</p>
		{/if}
	{:else}
		<p role="status" class="assist-note">
			{status === 'none' ? m.search_assist_natural_none() : m.search_assist_natural_failed()}
			{reason}
		</p>
	{/if}
	<div class="assist-row" bind:this={actions}>
		{#if status === 'ready' && proposal}
			<Button variant="primary" onclick={() => proposal && apply(proposal.canonical)}
				>{m.search_assist_natural_apply()}</Button
			>
		{/if}
		<Button onclick={dismiss}>{m.search_assist_natural_dismiss()}</Button>
	</div>
</section>
