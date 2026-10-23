<script lang="ts">
	// The search box at the top of the navigation (§10.10.3): a combobox over a listbox of results.
	// Only this small shell is in the route; the index, the matcher and the results load the first
	// time the box is focused. `/` focuses it while on a Settings page.
	import { goto } from '$app/navigation';
	import { api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import type { Component } from 'svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import type { SectionValues } from '../form/types';
	import type { Hit, SearchIndex } from './search';
	import { useSettings } from './state.svelte';

	const settings = useSettings();

	let query = $state('');
	let open = $state(false);
	let hits = $state.raw<Hit[]>([]);
	let activeId = $state<string>();
	let index = $state.raw<SearchIndex>();
	let Results = $state.raw<Component<ResultsProps>>();
	let values = $state.raw<Record<string, SectionValues | undefined>>({});
	let input = $state<HTMLInputElement>();
	let loading: Promise<void> | undefined;

	type ResultsProps = import('svelte').ComponentProps<
		typeof import('./SearchResults.svelte').default
	>;
	const LIMIT = 12;

	/** Builds the index on first use: every screen's text loads first. */
	function load(): Promise<void> {
		loading ??= (async () => {
			const [search, results, { loadSettingMessages }] = await Promise.all([
				import('./search'),
				import('./SearchResults.svelte'),
				import('../form/messages')
			]);
			const sections = settings.index?.sections.map((s) => s.section) ?? [];
			await loadSettingMessages(sections);
			index = search.createIndex(
				search.buildItems(settings.registry),
				document.documentElement.lang || 'en'
			);
			Results = results.default;
		})();
		return loading;
	}

	/** Saved values for the sections the hits are in: each fetched once while the box is open. */
	const asked = new SvelteSet<string>();
	async function fetchValues(list: Hit[]) {
		for (const section of new Set(
			list.flatMap((h) => (h.item.spec ? [h.item.spec.section] : []))
		)) {
			if (asked.has(section)) continue;
			asked.add(section);
			try {
				const v = await api.get('/settings/{section}', { path: { section } });
				values = { ...values, [section]: v };
			} catch {
				asked.delete(section);
			}
		}
	}

	function run() {
		hits = query.trim() && index ? index.search(query).slice(0, LIMIT) : [];
		activeId = hits[0]?.item.id;
		if (hits.length) void fetchValues(hits);
	}

	function close() {
		open = false;
		asked.clear();
		values = {};
	}

	function pick(hit: Hit) {
		const hash = hit.item.anchor ? `#${hit.item.anchor}` : '';
		close();
		query = '';
		hits = [];
		// eslint-disable-next-line svelte/no-navigation-without-resolve -- no base path (svelte.config.js)
		void goto(`/settings/${hit.item.screenId}${hash}`);
	}

	function onkeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			if (query) query = '';
			else input?.blur();
			run();
			close();
			return;
		}
		if (!hits.length) return;
		const at = hits.findIndex((h) => h.item.id === activeId);
		if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
			e.preventDefault();
			open = true;
			const next = (at + (e.key === 'ArrowDown' ? 1 : -1) + hits.length) % hits.length;
			activeId = hits[next]!.item.id;
			document
				.getElementById(`settings-search-${activeId}`)
				?.scrollIntoView?.({ block: 'nearest' });
		} else if (e.key === 'Enter' && open && at >= 0) {
			e.preventDefault();
			pick(hits[at]!);
		}
	}

	const guideUrl = $derived(
		settings.system?.repository ? `${settings.system.repository}/tree/main/docs/user` : undefined
	);
	const showResults = $derived(
		open && query.trim() !== '' && Results !== undefined && index !== undefined
	);
</script>

<div class="settings-search" data-testid="settings-search">
	<label class="sr-only" for="settings-search">{m.settings_ui_search_label()}</label>
	<input
		id="settings-search"
		bind:this={input}
		bind:value={query}
		type="search"
		role="combobox"
		autocomplete="off"
		spellcheck="false"
		class="input w-full border-field-border"
		placeholder={m.settings_ui_search_placeholder()}
		aria-expanded={showResults}
		aria-controls="settings-search-results"
		aria-autocomplete="list"
		aria-activedescendant={showResults && activeId ? `settings-search-${activeId}` : undefined}
		onfocus={() => {
			open = true;
			void load().then(run);
		}}
		onblur={close}
		oninput={() => {
			open = true;
			run();
		}}
		{onkeydown}
	/>
	<div id="settings-search-results">
		{#if showResults && Results}
			<Results
				{hits}
				{query}
				{activeId}
				{values}
				{guideUrl}
				onpick={pick}
				onactive={(id) => (activeId = id)}
			/>
		{/if}
	</div>
</div>

<style>
	.settings-search {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 2);
	}
</style>
