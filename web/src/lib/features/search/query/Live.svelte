<script lang="ts">
	// The query bar's lazy part (QueryBar loads it on the first interaction or a `q` in the URL):
	// the editor, the search state, the parse-error line, the "couldn't check" banner and the
	// active-filter strip (itself lazy until there is a chip). The URL is the state (§10.3): every
	// write goes through W5, after `ensureSearchPath()`.
	import { onDestroy, onMount, untrack } from 'svelte';
	import IconError from '~icons/ph/warning-circle';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import { search, type Ast } from '$lib/state/search.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { chipClauses, rootWithout, withoutFilters, type Clause } from './clauses';
	import { attach, type Editor } from './editor';
	import { ensureSearchPath } from './path';
	import { errorRange } from './spans';
	import type { SearchState, SyntaxComponent } from './types';

	interface Props {
		el: HTMLElement;
		searchState?: SearchState;
		/** The message line's id (the editor's aria-describedby). */
		message: string;
		onescape: () => boolean;
		onempty: (empty: boolean) => void;
		/** True once if Enter was pressed before this part loaded. */
		takeSubmit: () => boolean;
		/** An IME composition is open right now (the static bar tracks it from first paint). */
		composing: () => boolean;
	}
	let { el, searchState, message, onescape, onempty, takeSubmit, composing }: Props = $props();
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
		const midComposition = composing();
		editor = attach(el, {
			onchange: setText,
			onsubmit: submit,
			onescape,
			onrender: (text) => onempty(!text),
			composing: midComposition
		});
		// Whatever was typed before this loaded. After the URL sync above (effects run in order), so
		// the URL's empty query does not wipe it. Mid-composition, compositionend reports it instead.
		if (!midComposition) {
			if (editor.text && editor.text !== s.text) setText(editor.text);
			else if (s.text) editor.set(s.text, true); // the URL's query, adopted before the editor existed
		}
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

	// The strip explains the results, which still show the last query that parsed: keep its AST
	// while an error is showing (W5 clears `ast` on an error).
	let shown = $state.raw<Ast | null>(null);
	$effect(() => {
		const ast = s.ast;
		if (ast) shown = ast;
		else if (!s.parseError && !s.parsing) shown = null;
	});
	const clauses = $derived(chipClauses(shown));
	const hidden = $derived(s.hidden);

	let Strip = $state<typeof import('./FilterStrip.svelte').default>();
	$effect(() => {
		if ((clauses.length || hidden) && !Strip)
			void import('./FilterStrip.svelte').then((mod) => (Strip = mod.default));
	});

	// Polite announcements ("Removed filter: …"). Cleared first so a repeat is read again.
	let said = $state('');
	function announce(text: string) {
		said = '';
		requestAnimationFrame(() => (said = text));
	}

	async function remove(clause: Clause) {
		if (!shown) return;
		ensureSearchPath();
		await s.setAst(rootWithout(shown, clause.index));
		announce(m.search_filters_removed({ label: clause.label.text }));
	}

	async function removeHidden(label: string) {
		ensureSearchPath();
		await s.setHidden(false);
		announce(m.search_filters_removed({ label }));
	}

	async function clearAll() {
		ensureSearchPath();
		if (clauses.length) await s.setAst(withoutFilters(shown));
		if (s.hidden) await s.setHidden(false);
		announce(m.search_filters_cleared());
	}

	export function tryQuery(query: string) {
		setText(query);
		submit();
		editor?.focusEnd();
	}

	export function focusEnd() {
		editor?.focusEnd();
	}

	export async function loadSyntax(): Promise<SyntaxComponent> {
		return (await import('./SyntaxPopover.svelte')).default;
	}
</script>

<!-- One text-small line, always (nothing shifts on the bare page); an error's text wraps rather than
     being cut, so its "what to do" part stays readable at 320 px (WCAG 1.4.10). -->
<p id={message} data-live role="status" class="text-small" class:text-error={!!syntax}>
	{#if syntax}
		<IconError class="qt-icon inline size-4" aria-hidden="true" />
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
{#if Strip && (clauses.length || hidden)}
	<Strip
		{clauses}
		{hidden}
		{el}
		onremove={remove}
		onremovehidden={removeHidden}
		onclear={clearAll}
	/>
{/if}
<p class="sr-only" aria-live="polite">{said}</p>
