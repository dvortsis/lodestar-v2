<script lang="ts">
	// Zero-result rescue (§10.4 "No results", U8, U9): shown only for an empty answer. It offers, and
	// never runs anything itself: "Did you mean `seeders:>5`?" for a filter typed as words, and the
	// natural-phrasing chips with Apply when the words read as filters. Each question to the server
	// is asked once per query text, and an answer for older text is dropped.
	import { api, isAbort } from '$lib/api/client';
	import type { components } from '$lib/api/schema';
	import { m } from '$lib/paraglide/messages.js';
	import { search } from '$lib/state/search.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { correct } from '../assist/rescue';
	import { ensureSearchPath } from '../query/path';

	interface Props {
		/** True while a "Did you mean" shows, so the panel doesn't also say "try fewer words". */
		suggesting?: boolean;
	}
	let { suggesting = $bindable(false) }: Props = $props();

	type Natural = NonNullable<components['schemas']['ParseResponse']['natural']>;
	let didYouMean = $state<{ text: string; clauses: string[] } | null>(null);
	let natural = $state.raw<Natural | null>(null);
	let said = $state('');

	const asked = $derived(search.q);

	$effect(() => {
		const q = asked;
		didYouMean = null;
		natural = null;
		suggesting = false;
		said = '';
		if (!q.trim()) return;
		const ctl = new AbortController();
		const signal = ctl.signal;
		void (async () => {
			try {
				const { fields } = await api.get('/search/fields', { signal });
				const fix = correct(q, fields);
				if (fix) {
					// Only a correction the server's own parser accepts is offered.
					await api.get('/parse', { query: { q: fix.text }, signal });
					if (signal.aborted) return;
					didYouMean = fix;
					suggesting = true;
					said = m.search_rescue_didyoumean({ query: fix.text });
				}
			} catch (e) {
				if (isAbort(e)) return;
			}
			try {
				const r = await api.get('/parse', { query: { q, natural: true }, signal });
				if (signal.aborted) return;
				if (r.natural?.chips.length && r.natural.canonical !== q) natural = r.natural;
			} catch {
				// no rescue offered
			}
		})();
		return () => {
			ctl.abort();
			suggesting = false;
		};
	});

	function run(text: string) {
		ensureSearchPath();
		search.setText(text);
		void search.submit();
	}
</script>

{#if didYouMean}
	<p>
		<Button data-testid="rescue-didyoumean" onclick={() => run(didYouMean!.text)}
			>{m.search_rescue_didyoumean({ query: didYouMean.text })}</Button
		>
	</p>
{/if}
{#if natural}
	<div data-testid="rescue-natural">
		<p>{m.search_rescue_natural()}</p>
		<ul class="flex flex-wrap gap-2" aria-label={m.search_rescue_chips()}>
			{#each natural.chips as chip (chip.text + chip.from.start)}
				<li
					class="rounded-selector border border-field-border bg-base-100 px-2 font-mono text-small text-base-content"
				>
					{chip.text}
				</li>
			{/each}
		</ul>
		<Button variant="primary" onclick={() => run(natural!.canonical)}
			>{m.search_rescue_natural_apply()}</Button
		>
	</div>
{/if}
<p class="sr-only" role="status">{said}</p>
