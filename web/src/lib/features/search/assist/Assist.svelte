<script lang="ts">
	// The bar's autocomplete (§10.4, §10.13): a WAI-ARIA combobox/listbox over the editor, which
	// keeps DOM focus. Arrow keys move through the options (aria-activedescendant), Tab accepts
	// (the active option, else the first), Enter accepts only an option the user moved to (else it
	// searches), Esc closes. The editor's own keydown never sees a key the listbox took.
	import { onDestroy, onMount, untrack } from 'svelte';
	import { api } from '$lib/api/client';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import {
		applyOption,
		buildOptions,
		charPos,
		type Option,
		type Saved,
		type SuggestItem,
		type SuggestKind
	} from './options';
	import { loadRecent } from './recent';
	import { createSuggester } from './suggest';

	interface Props {
		el: HTMLElement;
		/** The editor's text. */
		text: () => string;
		/** Puts `text` in the bar (and the search state); `run` also searches it. */
		accept: (text: string, run: boolean) => void;
		/** The existing polite region. */
		announce: (text: string) => void;
		hidden?: () => boolean;
	}
	let { el, text, accept, announce, hidden }: Props = $props();

	const uid = $props.id();
	const LISTBOX = `search-assist-${uid}`;
	const optionId = (o: Option) => `${LISTBOX}-${o.id}`;

	let items = $state.raw<SuggestItem[]>([]);
	/** The text `items` were asked for: a list for older text must not be accepted into newer. */
	let itemsFor = $state('');
	let recent = $state.raw<string[]>([]);
	let saved = $state.raw<Saved[]>([]);
	let savedLoaded = false;
	let current = $state('');
	let open = $state(false);
	let active = $state(-1);
	/** The user has typed since the list opened: only then does Tab accept an unchosen option. */
	let typed = false;

	const options = $derived(buildOptions(current, itemsFor === current ? items : [], recent, saved));
	const shown = $derived(open && options.length > 0);

	const suggester = createSuggester(
		(got, forQuery) => {
			items = got;
			itemsFor = forQuery;
			active = -1; // the list under the highlight changed: it must not land on another option
		},
		() => hidden?.() ?? false
	);

	/** The caret, in UTF-16 units over the bar's text. */
	function caret(): number {
		const sel = document.getSelection();
		if (!sel?.rangeCount || !sel.focusNode || !el.contains(sel.focusNode)) return current.length;
		const r = document.createRange();
		r.selectNodeContents(el);
		r.setEnd(sel.focusNode, sel.focusOffset);
		return r.toString().length;
	}

	async function loadSaved() {
		if (savedLoaded) return;
		savedLoaded = true;
		try {
			saved = (await api.get('/saved-searches')).map((s) => ({ name: s.name, q: s.q }));
		} catch {
			savedLoaded = false; // offline or signed out: try again at the next opening
		}
	}

	function show() {
		current = text();
		recent = loadRecent();
		open = true;
		active = -1;
		void loadSaved();
	}

	function oninput() {
		show();
		typed = true;
		if (current.trim()) suggester.request(current, charPos(current, caret()));
		else {
			suggester.cancel();
			items = [];
		}
	}

	function close() {
		open = false;
		active = -1;
		typed = false;
		suggester.cancel();
	}

	function pick(o: Option) {
		let next = applyOption(text(), o);
		// A value or title is a finished word: the caret moves on to the next one.
		if (!o.run && o.kind !== 'field' && !next.endsWith(' ')) next += ' ';
		close();
		items = [];
		accept(next, o.run);
	}

	function onkeydown(e: KeyboardEvent) {
		if (e.isComposing || e.keyCode === 229) return;
		// Enter searches unless the user moved to an option: the suggestions are done, even ones still
		// on their way (a late answer must not reopen the list under a Tab that means "leave").
		if (e.key === 'Enter' && !(shown && active >= 0)) return close();
		if (!shown) {
			if (e.key === 'ArrowDown' && !e.altKey) {
				e.preventDefault();
				e.stopImmediatePropagation();
				show();
				active = 0;
			}
			return;
		}
		const take = () => {
			e.preventDefault();
			e.stopImmediatePropagation();
		};
		const n = options.length;
		if (e.key === 'ArrowDown') {
			take();
			active = (active + 1) % n;
		} else if (e.key === 'ArrowUp') {
			take();
			active = active <= 0 ? n - 1 : active - 1;
		} else if (e.key === 'Escape') {
			take();
			close();
		} else if (
			e.key === 'Tab' &&
			!e.shiftKey &&
			!e.ctrlKey &&
			!e.metaKey &&
			(typed || active >= 0)
		) {
			take();
			pick(options[Math.max(active, 0)]!);
		} else if (e.key === 'Enter') {
			take();
			pick(options[active]!);
		}
	}

	onMount(() => {
		el.addEventListener('keydown', onkeydown, true); // before the editor's own handler
		el.addEventListener('input', oninput);
		el.addEventListener('focus', onfocus);
		el.addEventListener('blur', close);
		if (document.activeElement === el) {
			if (text()) oninput();
			else show();
		}
	});
	function onfocus() {
		if (!text()) show();
	}
	onDestroy(() => {
		el.removeEventListener('keydown', onkeydown, true);
		el.removeEventListener('input', oninput);
		el.removeEventListener('focus', onfocus);
		el.removeEventListener('blur', close);
		suggester.cancel();
		el.setAttribute('aria-expanded', 'false');
		el.removeAttribute('aria-controls');
		el.removeAttribute('aria-activedescendant');
	});

	$effect(() => {
		el.setAttribute('aria-expanded', String(shown));
		if (shown) el.setAttribute('aria-controls', LISTBOX);
		else el.removeAttribute('aria-controls');
		const o = shown ? options[active] : undefined;
		if (o) el.setAttribute('aria-activedescendant', optionId(o));
		else el.removeAttribute('aria-activedescendant');
	});

	// "n suggestions" when the list opens or its size changes, not on every arrow key.
	let announced = 0;
	$effect(() => {
		const n = shown ? options.length : 0;
		untrack(() => {
			if (n && n !== announced) announce(m.search_assist_count({ count: n }));
			announced = n;
		});
	});

	const KIND: Record<SuggestKind, () => string> = {
		field: m.search_assist_kind_field,
		value: m.search_assist_kind_value,
		title: m.search_assist_kind_title,
		chip: m.search_assist_kind_chip,
		did_you_mean: m.search_assist_kind_did_you_mean,
		recent: m.search_assist_kind_recent,
		saved: m.search_assist_kind_saved
	};
</script>

{#if shown}
	<!-- Options are chosen from the editor (the combobox keeps focus), so a pointer press must not
	     move focus off it; the click then picks. -->
	<ul
		id={LISTBOX}
		role="listbox"
		aria-label={m.search_assist_label()}
		class="assist-pop"
		onpointerdown={(e) => e.preventDefault()}
	>
		{#each options as o, i (o.id)}
			<!-- svelte-ignore a11y_click_events_have_key_events -->
			<li
				id={optionId(o)}
				role="option"
				aria-selected={i === active}
				class="assist-option"
				onclick={() => pick(o)}
			>
				<span class="assist-kind">{KIND[o.kind]()}</span>
				<span class="assist-text" class:assist-mono={o.kind !== 'title' && o.kind !== 'saved'}
					>{o.text}</span
				>
				{#if o.count !== undefined}
					<span class="assist-count" data-testid="assist-count"
						>{#if o.approx}<span aria-hidden="true"
								>{formatCount({ value: o.count, approx: true }, currentLocale())}</span
							><span class="sr-only"
								>{m.search_assist_count_about({
									count: formatCount(o.count, currentLocale())
								})}</span
							>{:else}{formatCount(o.count, currentLocale())}{/if}</span
					>
				{/if}
			</li>
		{/each}
	</ul>
{/if}
