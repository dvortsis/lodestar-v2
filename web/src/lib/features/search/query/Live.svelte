<script lang="ts">
	// The query bar's lazy part (QueryBar loads it on the first interaction or a `q` in the URL):
	// the editor, the search state, the parse-error line and the "couldn't check" banner. The URL
	// is the state (§10.3): every write goes through W5, after `ensureSearchPath()`.
	import { onDestroy, onMount, untrack } from 'svelte';
	import IconError from '~icons/ph/warning-circle';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import { search } from '$lib/state/search.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { attach, type Editor } from './editor';
	import { ensureSearchPath } from './path';
	import { errorRange } from './spans';
	import type { SearchState } from './types';

	interface Props {
		el: HTMLElement;
		searchState?: SearchState;
		/** The message line's id (the editor's aria-describedby). */
		message: string;
		onescape: () => boolean;
		onempty: (empty: boolean) => void;
		/** True once if Enter was pressed before this part loaded. */
		takeSubmit: () => boolean;
	}
	let { el, searchState, message, onescape, onempty, takeSubmit }: Props = $props();
	const s = $derived(searchState ?? search);

	let editor: Editor | undefined;

	function setText(text: string) {
		ensureSearchPath();
		s.setText(text);
	}

	function submit() {
		ensureSearchPath();
		void s.submit();
	}

	// The URL is the state: adopt outside changes (back/forward, a pasted link, a facet click).
	// An empty query in an empty bar has nothing to adopt or parse: no request for the bare page.
	$effect(() => {
		const q = page.url.searchParams.get('q');
		untrack(() => {
			if (q || s.text) s.sync();
		});
	});

	// The state's text (the URL's query, the canonical text after Enter or a chip) shows in the bar.
	$effect(() => {
		const text = s.text;
		untrack(() => {
			if (editor && text !== editor.text) editor.set(text);
		});
	});

	onMount(() => {
		void import('./bar.css');
		editor = attach(el, {
			onchange: setText,
			onsubmit: submit,
			onescape,
			onrender: (text) => onempty(!text)
		});
		// Whatever was typed before this loaded. After the URL sync above (effects run in order), so
		// the URL's empty query does not wipe it.
		if (editor.text && editor.text !== s.text) setText(editor.text);
		else if (s.text) editor.set(s.text); // the URL's query, adopted before the editor existed
		if (takeSubmit()) submit();
	});
	onDestroy(() => editor?.destroy());

	const blank = $derived(!s.text.trim());
	/** Still the text that was parsed: on an error W5 keeps the raw text in the URL. */
	const settled = $derived(!s.parsing && s.text === s.q);
	const syntax = $derived(
		!blank && settled && s.parseError?.code === 'query_syntax' ? s.parseError : null
	);
	// Any other failure (network, 5xx) is a banner; a 401 is the shell's (it goes to sign-in).
	const unchecked = $derived(
		!blank &&
			!s.parsing &&
			!!s.parseError &&
			s.parseError.code !== 'query_syntax' &&
			s.parseError.code !== 'login_required'
	);

	$effect(() => {
		const range = syntax ? errorRange(s.text, syntax.start, syntax.end) : null;
		untrack(() => editor?.setError(range));
		if (syntax) el.setAttribute('aria-invalid', 'true');
		else el.removeAttribute('aria-invalid');
	});

	export function focusEnd() {
		editor?.focusEnd();
	}
</script>

<p id={message} class="truncate text-small" class:text-error={!!syntax}>
	{#if syntax}
		<IconError class="qt-icon size-4" aria-hidden="true" />
		{m.search_query_error_lead()}
		{syntax.message}
	{:else}
		&nbsp;
	{/if}
</p>
{#if unchecked}
	<Banner kind="error" title={m.search_query_check_failed()}>
		{m.search_query_check_failed_body()}
		{#snippet action()}
			<Button dense onclick={submit}>{m.search_query_retry()}</Button>
		{/snippet}
	</Banner>
{/if}
