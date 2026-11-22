<script lang="ts">
	// The query bar (§10.4) and the search landmark around it. Only this markup is on the bare
	// page's first load (§10.1: the 102,400-byte Lighthouse budget had about 1 KB left): the field
	// takes typing natively until the first interaction, or a `q` in the URL, loads the editor, the
	// search state and the strip (`Live.svelte`), which then pick up whatever was typed.
	// No route stylesheet either (the 20 KB CSS budget had 0.18 KB left): first-paint geometry is
	// built from emitted utilities and token variables; everything else is in Live's stylesheet.
	import { onMount, tick } from 'svelte';
	import { page } from '$app/state';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import { m } from '$lib/paraglide/messages.js';
	import { onFirstInteraction } from './engaged';
	import type { LiveApi, SearchState, SyntaxComponent } from './types';

	interface Props {
		/** W5's search state; tests pass `createSearchState(nav)`. Defaults to `search`. */
		searchState?: SearchState;
	}
	let { searchState }: Props = $props();

	let el = $state<HTMLDivElement>();
	let empty = $state(true);
	let Live = $state<typeof import('./Live.svelte').default>();
	let live = $state<LiveApi>();
	let Syntax = $state<SyntaxComponent>();
	let syntaxOpen = $state(false);
	/** Enter pressed before the editor loaded: submitted once it has. */
	let pendingSubmit = false;
	/** An IME composition is open (tracked from first paint, for an editor that loads mid-way). */
	let composing = false;
	let loading: Promise<unknown> | undefined;

	function engage() {
		loading ??= import('./Live.svelte').then((mod) => (Live = mod.default));
		return loading;
	}

	onMount(() => {
		// Safari's `autocorrect`, which isn't in Svelte's attribute types (a spread would cost more).
		el!.setAttribute('autocorrect', 'off');
		return onFirstInteraction(engage);
	});

	// A query (or `hidden`) in the URL needs its text, checks and chips now: a pasted link, back.
	// `hidden` reads as W5's `readUrl` does ("1" or "true"); importing it would put W5 on the
	// first load.
	$effect(() => {
		const p = page.url.searchParams;
		const hidden = p.get('hidden');
		if (p.get('q') || p.get('browse') === '1' || hidden === '1' || hidden === 'true') void engage();
	});

	/** `/` (search.focus): focus with the caret at the end. */
	export function focusEnd() {
		if (live) return live.focusEnd();
		void engage();
		if (!el) return;
		el.focus();
		getSelection()?.collapse(el, el.childNodes.length);
	}

	/** Search's Retry: typed text the URL doesn't hold yet goes in first (none before the editor). */
	export function flush() {
		return live?.flush();
	}

	/** The lazy part, once mounted. */
	async function ready(): Promise<LiveApi | undefined> {
		await engage();
		await tick();
		return live;
	}

	async function openSyntax(event: MouseEvent) {
		const button = event.currentTarget as HTMLElement;
		let loaded: SyntaxComponent | undefined;
		try {
			loaded = await (await ready())?.loadSyntax();
		} catch {
			return; // the chunk didn't load: the button stays, and a press tries again
		}
		// Open only if the user is still at the button: a slow load mustn't pull focus from typing.
		const still = document.activeElement === button || document.activeElement === document.body;
		Syntax = loaded;
		if (still) syntaxOpen = true;
	}

	async function tryQuery(query: string) {
		(await ready())?.tryQuery(query);
	}

	/** The editor's Esc: an open popover closes first (S-search-4 adds its listbox here). */
	function onescape() {
		if (!syntaxOpen) return false;
		syntaxOpen = false;
		return true;
	}
</script>

<search
	data-search-bar
	class="flex flex-col gap-1 xx-large:max-w-6xl"
	aria-labelledby="search-query-label"
>
	<span id="search-query-label" class="sr-only">{m.search_query_label()}</span>
	<div class="flex items-center gap-2">
		<div class="relative min-w-0 flex-1">
			<!-- The listbox (S-search-4) attaches here: aria-controls, and aria-expanded while open.
			     ARIA 1.2 needs aria-controls only while the popup is shown, which it never is yet. -->
			<!-- svelte-ignore a11y_role_has_required_aria_props -->
			<div
				bind:this={el}
				role="combobox"
				contenteditable="plaintext-only"
				tabindex="0"
				aria-expanded="false"
				aria-autocomplete="list"
				aria-haspopup="listbox"
				aria-labelledby="search-query-label"
				aria-describedby="search-query-message"
				spellcheck="false"
				autocapitalize="off"
				enterkeyhint="search"
				inputmode="search"
				data-testid="query-editor"
				class="block w-full overflow-x-auto rounded-field border border-field-border bg-base-100 px-4 text-body-lg {FOCUS_RING}"
				style="height:var(--query-bar-height);padding-block:calc(var(--spacing) * 3 - var(--border));white-space:pre;overflow-y:hidden;scrollbar-width:none"
				oninput={() => (empty = !el?.textContent)}
				oncompositionstart={() => (composing = true)}
				oncompositionend={() => (composing = false)}
				onkeydown={(e) => {
					if (e.key === 'Enter' && !Live) {
						e.preventDefault();
						pendingSubmit = true;
					}
				}}
			></div>
			<!-- The placeholder: never the accessible name (that is the label). The short one fits a phone. -->
			<span
				aria-hidden="true"
				hidden={!empty}
				class="pointer-events-none absolute inset-0 truncate px-4 text-body-lg text-muted-text"
				style="padding-block:calc(var(--spacing) * 3)"
				><span class="medium:hidden">{m.search_query_placeholder_short()}</span><span
					class="hidden medium:inline">{m.search_query_placeholder()}</span
				></span
			>
		</div>
		{#if Syntax}
			<Syntax bind:open={syntaxOpen} ontry={tryQuery} />
		{:else}
			<button
				class="btn btn-ghost {FOCUS_RING} {TARGET_MIN}"
				aria-haspopup="dialog"
				onclick={openSyntax}>{m.search_query_syntax()}</button
			>
		{/if}
	</div>
	{#if Live && el}
		<Live
			bind:this={live}
			{el}
			{searchState}
			message="search-query-message"
			{onescape}
			onempty={(v: boolean) => (empty = v)}
			composing={() => composing}
			takeSubmit={() => {
				const was = pendingSubmit;
				pendingSubmit = false;
				return was;
			}}
		/>
	{:else}
		<!-- The message line's place: one text-small line, always, so nothing shifts. -->
		<p id="search-query-message" role="status" class="text-small">&nbsp;</p>
	{/if}
</search>
