<script lang="ts">
	// The query bar (§10.4) and the search landmark around it. Only this markup is on the bare
	// page's first load (§10.1: the 102,400-byte Lighthouse budget had about 1 KB left): the field
	// takes typing natively until the first interaction, or a `q` in the URL, loads the editor, the
	// search state and the strip (`Live.svelte`), which then pick up whatever was typed.
	// No route stylesheet either (the 20 KB CSS budget had 0.18 KB left): first-paint geometry is
	// built from emitted utilities and token variables; everything else is in Live's stylesheet.
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { FOCUS_RING } from '$lib/ui/classes';
	import { m } from '$lib/paraglide/messages.js';
	import { onFirstInteraction } from './engaged';
	import type { LiveApi, SearchState } from './types';

	interface Props {
		/** W5's search state; tests pass `createSearchState(nav)`. Defaults to `search`. */
		searchState?: SearchState;
	}
	let { searchState }: Props = $props();

	const LABEL = 'search-query-label';
	const MESSAGE = 'search-query-message';
	// Safari's `autocorrect` is not in Svelte's attribute types.
	const NO_AUTOCORRECT = { autocorrect: 'off' } as Record<string, string>;

	let el = $state<HTMLDivElement>();
	let empty = $state(true);
	let Live = $state<typeof import('./Live.svelte').default>();
	let live = $state<LiveApi>();
	/** Enter pressed before the editor loaded: submitted once it has. */
	let pendingSubmit = false;
	let loading: Promise<unknown> | undefined;

	function engage() {
		loading ??= import('./Live.svelte').then((mod) => (Live = mod.default));
		return loading;
	}

	onMount(() => onFirstInteraction(engage));

	// A query in the URL needs its text and its checks now: a pasted link, back.
	$effect(() => {
		const p = page.url.searchParams;
		if (p.get('q')) void engage();
	});

	/** `/` (search.focus): focus with the caret at the end. */
	export function focusEnd() {
		if (live) return live.focusEnd();
		void engage();
		if (!el) return;
		el.focus();
		getSelection()?.collapse(el, el.childNodes.length);
	}

	/** The editor's Esc: an open popup closes first (S-search-4's listbox will). */
	function onescape() {
		return false;
	}
</script>

<search data-search-bar class="flex flex-col gap-1 px-4 py-2" aria-labelledby={LABEL}>
	<span id={LABEL} class="sr-only">{m.search_query_label()}</span>
	<div class="flex items-center gap-2">
		<div class="relative min-w-0 flex-1">
			<!-- The listbox (S-search-4) attaches here: aria-controls, and aria-expanded while open.
			     ARIA 1.2 needs aria-controls only while the popup is shown, which it never is yet. -->
			<div
				bind:this={el}
				role="combobox"
				contenteditable="plaintext-only"
				tabindex="0"
				aria-expanded="false"
				aria-autocomplete="list"
				aria-haspopup="listbox"
				aria-labelledby={LABEL}
				aria-describedby={MESSAGE}
				spellcheck="false"
				autocapitalize="off"
				{...NO_AUTOCORRECT}
				enterkeyhint="search"
				inputmode="search"
				data-testid="query-editor"
				class="block w-full overflow-x-auto rounded-field border border-field-border bg-base-100 px-4 text-body-lg {FOCUS_RING}"
				style="height: var(--query-bar-height); padding-block: calc(var(--spacing) * 3 - var(--border)); white-space: pre; overflow-y: hidden; scrollbar-width: none"
				oninput={() => (empty = !el?.textContent)}
				onkeydown={(e) => {
					if (e.key === 'Enter' && !Live) {
						e.preventDefault();
						pendingSubmit = true;
					}
				}}
			></div>
			<!-- The placeholder: never the accessible name (that is the label). -->
			<span
				aria-hidden="true"
				hidden={!empty}
				class="pointer-events-none absolute inset-0 truncate px-4 text-body-lg text-muted-text"
				style="padding-block: calc(var(--spacing) * 3)">{m.search_query_placeholder()}</span
			>
		</div>
	</div>
	{#if Live && el}
		<Live
			bind:this={live}
			{el}
			{searchState}
			message={MESSAGE}
			{onescape}
			onempty={(v: boolean) => (empty = v)}
			takeSubmit={() => {
				const was = pendingSubmit;
				pendingSubmit = false;
				return was;
			}}
		/>
	{:else}
		<!-- The message line's place: one text-small line, always, so nothing shifts. -->
		<p id={MESSAGE} class="text-small">&nbsp;</p>
	{/if}
</search>
