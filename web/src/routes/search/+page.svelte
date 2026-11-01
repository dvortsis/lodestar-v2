<script lang="ts">
	// The search page shell (§10.4). Regions, each filled by its own slice:
	//   QueryBar (S-search-1): the search landmark, the query bar and the active-filter strip;
	//   #search-rail (S-search-2): the facet rail;
	//   #results (S-search-3's toolbar, then S-search-2's list);
	//   #search-drawer (S-detail): the detail drawer, mounted from a lazy glob below.
	// Regions stay empty, with no landmark role, until their slice fills them.
	import { onMount, type Component } from 'svelte';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import QueryBar from '$lib/features/search/query/QueryBar.svelte';
	import { registerSearchCommands } from '$lib/features/search/query/commands';
	import { onFirstInteraction } from '$lib/features/search/query/engaged';

	let bar = $state<ReturnType<typeof QueryBar>>();

	// `/` and the page's scope. `$lib/keys` arrives with the shell's first interaction; this page's
	// listener is added first, so the command exists when the shell replays that first key. With no
	// interaction, the shell attaches its keys after 30 s: register at the same moment, so the first
	// `/` after that finds the command too.
	const KEYS_AFTER_MS = 30_000;
	onMount(() => {
		let off: (() => void) | undefined;
		let gone = false;
		const stop = onFirstInteraction(() => {
			void import('$lib/keys').then(
				({ keys }) => {
					if (!gone) off = registerSearchCommands(keys, () => bar?.focusEnd());
				},
				() => {}
			); // Shortcuts only: search works without them.
		}, KEYS_AFTER_MS);
		return () => {
			gone = true;
			stop();
			off?.();
		};
	});

	// S-search-2's results: lazy, never on the bare first load. A query (or `hidden`) in the URL needs
	// them at once; otherwise the empty state comes with the first interaction, or after 30 s, as the
	// shell's chrome does (§10.1: the bare page is Lighthouse's). Their sheets come with them.
	// The facet rail's host comes with them; it loads the rail itself where the rail shows. So does
	// S-search-3's toolbar, which shows once there is a query.
	type Panel = typeof import('$lib/features/search/results/ResultsPanel.svelte').default;
	type Host = typeof import('$lib/features/search/facets/FacetsHost.svelte').default;
	type Bar = typeof import('$lib/features/search/toolbar/ResultsToolbar.svelte').default;
	let Results = $state<Panel>();
	let Toolbar = $state<Bar>();
	let Facets = $state<Host>();
	let loadingResults: Promise<unknown> | undefined;
	function loadResults() {
		loadingResults ??= Promise.all([
			import('$lib/features/search/results/ResultsPanel.svelte'),
			import('$lib/features/search/facets/FacetsHost.svelte'),
			import('$lib/features/search/results/row.css'),
			import('$lib/features/search/results/results.css'),
			import('$lib/features/search/toolbar/ResultsToolbar.svelte'),
			import('$lib/features/search/toolbar/toolbar.css')
		]).then(([panel, host, , , bar]) => {
			Results = panel.default;
			Facets = host.default;
			Toolbar = bar.default;
		});
	}
	$effect(() => {
		const p = page.url.searchParams;
		if (
			p.get('q') ||
			p.get('browse') === '1' ||
			p.get('hidden') === '1' ||
			p.get('hidden') === 'true'
		)
			loadResults();
	});
	onMount(() => onFirstInteraction(loadResults, KEYS_AFTER_MS));

	// "Skip to results" (WCAG 2.4.1): at x-large the facet rail comes before the results, a tab stop
	// per value. The link is the first focusable in the page content (after the shell's skip link and
	// navigation) and goes to the list's active row, or to the region itself while there is no list
	// (the empty state, a skeleton, an error).
	function skipToResults(event: MouseEvent) {
		event.preventDefault();
		const region = document.getElementById('results');
		if (!region) return;
		const row = region.querySelector<HTMLElement>('[role="grid"] [role="row"][tabindex="0"]');
		if (row) return row.focus();
		region.setAttribute('tabindex', '-1');
		region.focus();
	}

	// S-detail's drawer: opened with a shallow pushState carrying `detail` in page.state. The glob
	// is empty until `DetailHost.svelte` exists, so neither slice edits the other's files, and the
	// drawer never joins this route's first load.
	const hosts = import.meta.glob<{ default: Component }>(
		'/src/lib/features/detail/DetailHost.svelte'
	);
	const loadHost = Object.values(hosts)[0];
	const drawerOpen = $derived('detail' in page.state);
	let Host = $state<Component>();
	$effect(() => {
		if (drawerOpen && loadHost && !Host) void loadHost().then((mod) => (Host = mod.default));
	});
</script>

<div class="flex flex-col gap-2 pb-4">
	<!-- The shell's skip link's classes: visible on focus, and no new utilities in the global sheet. -->
	<a
		href="#results"
		class="sr-only rounded-field bg-primary px-4 py-2 text-primary-content focus:not-sr-only focus:fixed focus:start-2 focus:top-2 focus:z-50 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary focus-visible:outline-solid"
		data-testid="skip-to-results"
		onclick={skipToResults}>{m.search_page_skip_results()}</a
	>
	<h1 class="mt-4 px-4 text-display font-semibold">{m.search_page_title()}</h1>
	<QueryBar bind:this={bar} />
	<div class="search-grid px-4">
		<!-- S-search-2: the facet rail. -->
		<div id="search-rail">
			{#if Facets}<Facets />{/if}
		</div>
		<!-- S-search-3: the results toolbar; S-search-2: the results list. -->
		<div id="results">
			{#if Toolbar}<Toolbar />{/if}
			{#if Results}<Results focusBar={() => bar?.focusEnd()} />{/if}
		</div>
		<!-- S-detail: DetailHost renders here; it decides the drawer's position per breakpoint. -->
		<div id="search-drawer">
			{#if Host}<Host />{/if}
		</div>
	</div>
</div>
