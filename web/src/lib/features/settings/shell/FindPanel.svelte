<script lang="ts">
	// The results of Settings search (§10.10.3), shown in place of the screen list while the box has
	// text. Loaded when the box is first focused, with the index. The box itself stays in the shell;
	// this makes it a combobox over the list below (WAI-ARIA 1.2): ↑/↓ move, Enter opens, Esc clears.
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { onDestroy } from 'svelte';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { Route } from '$lib/shell/nav';
	import type { FieldKey } from '../form/types';
	import {
		describe,
		forgetValues,
		highlight,
		loadFindIndex,
		search,
		type Described,
		type FindEntry
	} from './find';

	interface Props {
		input: HTMLInputElement;
		query: string;
		/** Clears the box. */
		clear: () => void;
	}
	let { input, query = $bindable(), clear }: Props = $props();

	let index = $state.raw<FindEntry[]>();
	let failed = $state(false);
	let selected = $state(0);
	let values = $state.raw<Record<string, Described | undefined>>({});

	// The index is built for the locale in use, and tried again after a failure when the text changes.
	$effect(() => {
		void currentLocale();
		if (failed && !query.trim()) return;
		failed = false;
		let live = true;
		loadFindIndex().then(
			(entries) => live && (index = entries),
			() => live && (failed = true)
		);
		return () => {
			live = false;
		};
	});

	const results = $derived(index ? search(index, query) : []);
	const active = $derived(Math.min(selected, results.length - 1));
	const id = (i: number) => `find-option-${i}`;

	// Selection starts at the top whenever the text changes; an emptied box forgets the values read.
	$effect(() => {
		selected = 0;
		if (!query.trim()) {
			forgetValues();
			values = {};
		}
	});

	// The value of each result, read as the list changes.
	$effect(() => {
		const shown = results;
		let live = true;
		for (const e of shown) {
			if (!e.spec || e.spec.key in values) continue;
			void describe(e).then((d) => live && (values = { ...values, [e.spec!.key]: d }));
		}
		return () => {
			live = false;
		};
	});

	// The box becomes a combobox over this list while the panel is on the page.
	$effect(() => {
		input.setAttribute('role', 'combobox');
		input.setAttribute('aria-autocomplete', 'list');
		input.setAttribute('aria-controls', 'find-list');
		input.setAttribute('aria-expanded', String(results.length > 0));
		if (results.length) input.setAttribute('aria-activedescendant', id(active));
		else input.removeAttribute('aria-activedescendant');
	});
	onDestroy(() => {
		for (const a of [
			'role',
			'aria-autocomplete',
			'aria-controls',
			'aria-expanded',
			'aria-activedescendant'
		])
			input.removeAttribute(a);
		forgetValues();
	});

	let stopHighlight: (() => void) | undefined;
	onDestroy(() => stopHighlight?.());

	function open(entry: FindEntry) {
		const spec = entry.spec;
		stopHighlight?.();
		clear();
		void goto(resolve(entry.href as Route)).then(() => {
			if (spec) stopHighlight = highlight(spec.key as FieldKey);
		});
	}

	function onkeydown(e: KeyboardEvent) {
		if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
			if (!results.length) return;
			e.preventDefault();
			selected = (active + (e.key === 'ArrowDown' ? 1 : -1) + results.length) % results.length;
			queueMicrotask(() =>
				document.getElementById(id(selected))?.scrollIntoView?.({ block: 'nearest' })
			);
		} else if (e.key === 'Enter' && results[active]) {
			e.preventDefault();
			open(results[active]);
		} else if (e.key === 'Escape' && query) {
			e.preventDefault();
			clear();
		}
	}
	$effect(() => {
		input.addEventListener('keydown', onkeydown);
		return () => input.removeEventListener('keydown', onkeydown);
	});
</script>

<!-- One live region, always on the page, so a change in it is announced. -->
<p class="sr-only" role="status">
	{!query.trim() || !index || failed
		? ''
		: results.length
			? m.settings_find_count({ count: results.length })
			: m.settings_find_none({ query: query.trim() })}
</p>

{#if query.trim()}
	<div class="find-list">
		{#if failed}
			<p role="status" class="text-small">{m.settings_find_failed()}</p>
		{:else if !index}
			<p role="status" class="text-small text-muted-text">{m.settings_find_loading()}</p>
		{:else if results.length}
			<p class="sr-only" role="status">{m.settings_find_count({ count: results.length })}</p>
			<ul
				id="find-list"
				role="listbox"
				aria-label={m.settings_find_results()}
				class="flex flex-col gap-1"
				data-testid="find-list"
			>
				{#each results as entry, i (entry.href)}
					{@const d = entry.spec ? values[entry.spec.key] : undefined}
					<!-- Keyboard users drive the options from the box (aria-activedescendant). -->
					<!-- svelte-ignore a11y_click_events_have_key_events -->
					<li
						id={id(i)}
						role="option"
						aria-selected={i === active}
						data-testid="find-option"
						class="flex cursor-default flex-col gap-1 rounded-field px-3 py-2 {i === active
							? 'bg-selected'
							: 'hover:bg-hover'}"
						onclick={() => open(entry)}
						onpointermove={() => (selected = i)}
					>
						<span class="font-medium">{entry.label}</span>
						<span class="text-caption {i === active ? '' : 'text-muted-text'}">
							{entry.trail}
						</span>
						{#if d}
							<span class="min-w-0 text-small break-words"
								>{d.value}
								<span class={i === active ? '' : 'text-muted-text'}>· {d.source}</span></span
							>
						{/if}
					</li>
				{/each}
			</ul>
		{:else}
			<p class="text-small">{m.settings_find_none({ query: query.trim() })}</p>
		{/if}
	</div>
{/if}

<style>
	/* Below 1024 px the results sit right under the box, before what needs attention (SettingsShell). */
	.find-list {
		order: 1;
	}
</style>
