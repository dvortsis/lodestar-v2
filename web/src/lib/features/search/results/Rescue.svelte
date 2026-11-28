<script module lang="ts">
	import type { components } from '$lib/api/schema';

	type Natural = NonNullable<components['schemas']['ParseResponse']['natural']>;
	type Found = { fix: { text: string; clauses: string[] } | null; natural: Natural | null };

	// Asked once per query text, for the page's life and across remounts: every new zero-result
	// answer unmounts this block while the next search loads, so the cache lives in the module. The
	// field list is fetched once; the answers are kept for the last text.
	let fieldsOnce: Promise<components['schemas']['SearchField'][]> | undefined;
	let last: { q: string; found: Found } | undefined;

	/** For tests: forget what was asked. */
	export function resetRescue() {
		fieldsOnce = undefined;
		last = undefined;
	}

	/** How long the rescue may take before the panel stops waiting for it. */
	export const RESCUE_LIMIT_MS = 4000;
</script>

<script lang="ts">
	// Zero-result rescue (§10.4 "No results", U8, U9): shown only for an empty answer. It offers, and
	// never runs anything itself: "Did you mean `seeders:>5`?" for a filter typed as words, and the
	// natural-phrasing chips with Apply when the words read as filters. Each question to the server
	// is asked once per query text, and an answer for older text is dropped.
	import { api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import { search } from '$lib/state/search.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Chip from '$lib/ui/labels/Chip.svelte';
	import { correct } from '../assist/rescue';
	import { ensureSearchPath } from '../query/path';

	interface Props {
		/** True while a "Did you mean" shows, so the panel doesn't also say "try fewer words". */
		suggesting?: boolean;
		/** False from a new query until the rescue has finished asking: the panel holds its hint. */
		settled?: boolean;
		/** The longest the rescue waits for the server (a prop for tests). */
		limit?: number;
	}
	let {
		suggesting = $bindable(false),
		settled = $bindable(false),
		limit: limitMs = RESCUE_LIMIT_MS
	}: Props = $props();

	const uid = $props.id();
	let didYouMean = $state<Found['fix']>(null);
	let natural = $state.raw<Natural | null>(null);
	let said = $state('');

	const asked = $derived(search.q);

	/** The corrected clause, when the server's own parser accepts it. Never throws: no offer. */
	async function fix(q: string, signal: AbortSignal): Promise<Found['fix']> {
		try {
			fieldsOnce ??= api
				.get('/search/fields', { signal })
				.then((r) => r.fields)
				.catch((e) => {
					fieldsOnce = undefined;
					throw e;
				});
			const found = correct(q, await fieldsOnce);
			if (!found) return null;
			await api.get('/parse', { query: { q: found.text }, signal });
			return found;
		} catch {
			return null;
		}
	}

	async function chips(q: string, signal: AbortSignal): Promise<Natural | null> {
		try {
			const r = await api.get('/parse', { query: { q, natural: true }, signal });
			return r.natural?.chips.length && r.natural.canonical !== q ? r.natural : null;
		} catch {
			return null;
		}
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
			await null; // the cached path settles after the panel's own reset for this text
			let found = last?.q === q ? last.found : null;
			if (!found) {
				// Past the limit the requests stop and the rescue settles with no offer.
				const limit = AbortSignal.any([ctl.signal, AbortSignal.timeout(limitMs)]);
				const [f, n] = await Promise.all([fix(q, limit), chips(q, limit)]);
				if (ctl.signal.aborted) return; // a newer text owns the block
				found = { fix: f, natural: n };
				if (!limit.aborted) last = { q, found };
			}
			if (ctl.signal.aborted) return;
			didYouMean = found.fix;
			natural = found.natural;
			suggesting = !!found.fix;
			if (found.fix) said = m.search_rescue_didyoumean({ query: found.fix.text });
			else if (found.natural) {
				const count = found.natural.chips.length;
				said = (count === 1 ? m.search_rescue_chips_ready_one : m.search_rescue_chips_ready_other)({
					count
				});
			}
			settled = true;
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

	// The query sits in mono inside the locale's own sentence: split it around a marker.
	const MARK = '\u0000';
	const around = $derived(m.search_rescue_didyoumean({ query: MARK }).split(MARK));
</script>

{#if didYouMean}
	<div class="flex flex-col items-start gap-2" data-testid="rescue-didyoumean">
		<p id="{uid}-dym">
			{around[0]}<span class="font-mono">{didYouMean.text}</span>{around[1] ?? ''}
		</p>
		<Button
			data-testid="rescue-didyoumean-apply"
			aria-describedby="{uid}-dym"
			onclick={() => run(didYouMean!.text)}>{m.search_rescue_apply()}</Button
		>
	</div>
{/if}
{#if natural}
	<div class="flex flex-col items-start gap-2" data-testid="rescue-natural">
		<p>{m.search_rescue_natural()}</p>
		<ul class="flex flex-wrap gap-2" aria-label={m.search_rescue_chips()}>
			{#each natural.chips as chip (chip.text + chip.from.start)}
				<li><Chip><span class="font-mono">{chip.text}</span></Chip></li>
			{/each}
		</ul>
		<Button variant="primary" onclick={() => run(natural!.canonical)}
			>{m.search_rescue_apply()}</Button
		>
	</div>
{/if}
<p class="sr-only" role="status">{said}</p>
