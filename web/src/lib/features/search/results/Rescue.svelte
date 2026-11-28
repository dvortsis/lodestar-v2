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
		/** False from a new query until the rescue has finished asking: the panel holds its hint. */
		settled?: boolean;
	}
	let { suggesting = $bindable(false), settled = $bindable(false) }: Props = $props();

	type Natural = NonNullable<components['schemas']['ParseResponse']['natural']>;
	type Found = { fix: { text: string; clauses: string[] } | null; natural: Natural | null };
	let didYouMean = $state<Found['fix']>(null);
	let natural = $state.raw<Natural | null>(null);
	let said = $state('');

	// Asked once per query text: the field list for the page's life, the answers for the last text,
	// so a remount for the same text (a retry, a facet change) asks nothing.
	let fieldsOnce: Promise<components['schemas']['SearchField'][]> | undefined;
	let last: { q: string; found: Found } | undefined;

	const asked = $derived(search.q);

	async function ask(q: string, signal: AbortSignal): Promise<Found> {
		let fix: Found['fix'] = null;
		let chips: Natural | null = null;
		try {
			fieldsOnce ??= api
				.get('/search/fields', {})
				.then((r) => r.fields)
				.catch((e) => {
					fieldsOnce = undefined;
					throw e;
				});
			const found = correct(q, await fieldsOnce);
			if (found) {
				// Only a correction the server's own parser accepts is offered.
				await api.get('/parse', { query: { q: found.text }, signal });
				fix = found;
			}
		} catch (e) {
			if (isAbort(e)) throw e;
		}
		try {
			const r = await api.get('/parse', { query: { q, natural: true }, signal });
			if (r.natural?.chips.length && r.natural.canonical !== q) chips = r.natural;
		} catch (e) {
			if (isAbort(e)) throw e;
		}
		return { fix, natural: chips };
	}

	$effect(() => {
		const q = asked;
		didYouMean = null;
		natural = null;
		suggesting = false;
		settled = false;
		said = '';
		if (!q.trim()) return;
		const ctl = new AbortController();
		void (async () => {
			try {
				const found = last?.q === q ? last.found : await ask(q, ctl.signal);
				if (ctl.signal.aborted) return;
				last = { q, found };
				didYouMean = found.fix;
				natural = found.natural;
				suggesting = !!found.fix;
				if (found.fix) said = m.search_rescue_didyoumean({ query: found.fix.text });
				else if (found.natural)
					said = m.search_rescue_chips_ready({ count: found.natural.chips.length });
			} catch {
				// aborted by a newer text, or nothing to offer
			}
			if (!ctl.signal.aborted) settled = true;
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
		// The block goes away with the new search: focus goes to the bar, not to the page.
		document.querySelector<HTMLElement>('[data-testid="query-editor"]')?.focus();
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
