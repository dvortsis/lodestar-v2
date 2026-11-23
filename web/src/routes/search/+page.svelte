<script lang="ts">
	// The search page shell (§10.4). Regions, each filled by its own slice:
	//   QueryBar (S-search-1): the search landmark, the query bar and the active-filter strip;
	//   #search-rail (S-search-2): the facet rail;
	//   #results (S-search-3's toolbar, then S-search-2's list);
	//   #search-drawer (S-detail): the detail drawer, a lazy import below.
	// Regions stay empty, with no landmark role, until their slice fills them.
	import { onMount, type Component } from 'svelte';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import QueryBar from '$lib/features/search/query/QueryBar.svelte';
	import { registerSearchCommands } from '$lib/features/search/query/commands';
	import { onFirstInteraction } from '$lib/features/search/query/engaged';
	import { checkFirstRun } from '$lib/features/auth/first-check';
	import { PAGE_FRAME, PAGE_BODY_FULL, PAGE_H1 } from '$lib/ui/layout/classes';

	let bar = $state<ReturnType<typeof QueryBar>>();

	// `/` and the page's scope. `$lib/keys` arrives with the shell's first interaction; this page's
	// listener is added first, so the command exists when the shell replays that first key. With no
	// interaction, the shell attaches its keys after 30 s: register at the same moment, so the first
	// `/` after that finds the command too.
	// Then the first-run check: a fresh instance, a signed-out visitor or a pending Legal Notice
	// can't search yet, so one check after mount sends the browser to setup or sign-in
	// (X-first-run-search-gate). It has no imports, so the bare first load gains one small
	// response; navigation loads only to leave. (One mount for both: the first load's bytes.)
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
		void checkFirstRun(() => !gone && location.pathname === '/search');
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
	type HomeComponent = typeof import('$lib/features/search/home/Home.svelte').default;
	type Bar = typeof import('$lib/features/search/toolbar/ResultsToolbar.svelte').default;
	let Results = $state<Panel>();
	let Toolbar = $state<Bar>();
	let Facets = $state<Host>();
	let Home = $state<HomeComponent>();
	// A failed set shows an alert in #results. It clears the cached promise, so a new query calls
	// `import()` again (Chromium 157 refetches; 153 keeps the failure for the module graph). Retry
	// reloads the page at the current URL, as Insights does. The tab's `window.name` carries a
	// one-shot marker across the reload, never into the URL; it is read and cleared here, and the
	// load it starts at mount sends focus to the results, if nothing else has focus. No later load
	// (the 30 s timer, a browse-mode key) can use it. The alert node stays while a retry fails
	// again, so it is announced once. Retry writes the bar's typed text into the URL first.
	let retried = window.name == 'retry';
	if (retried) window.name = '';
	let loadingResults: Promise<unknown> | undefined;
	let failed = $state<boolean>();
	function loadResults() {
		loadingResults ??= Promise.all([
			import('$lib/features/search/results/ResultsPanel.svelte'),
			import('$lib/features/search/facets/FacetsHost.svelte'),
			import('$lib/features/search/results/row.css'),
			import('$lib/features/search/results/results.css'),
			import('$lib/features/search/toolbar/ResultsToolbar.svelte'),
			import('$lib/features/search/toolbar/toolbar.css'),
			import('$lib/features/search/home/Home.svelte')
		]).then(
			([panel, host, , , bar, , home]) => {
				Home = home.default;
				Results = panel.default;
				Facets = host.default;
				Toolbar = bar.default;
				// Only while nothing has focus: a slow load never pulls it from typing (QueryBar's rule).
				if (retried) setTimeout(() => document.activeElement == document.body && focusResults());
				retried = false;
			},
			() => {
				loadingResults = undefined;
				retried = false;
				failed = true;
			}
		);
	}
	async function reload() {
		await bar?.flush();
		// Left Search while the flush waited (up to 1 s): no reload, and no marker to leak.
		if (location.pathname == '/search') {
			window.name = 'retry';
			location.reload();
		}
	}
	$effect(() => {
		// QueryBar's own test, written the same way so the two compress together.
		const p = page.url.searchParams;
		const hidden = p.get('hidden');
		if (p.get('q') || p.get('browse') === '1' || hidden === '1' || hidden === 'true') loadResults();
	});
	onMount(() => {
		if (retried) loadResults();
		return onFirstInteraction(loadResults, KEYS_AFTER_MS);
	});

	// "Skip to results" (WCAG 2.4.1): at x-large the facet rail comes before the results, a tab stop
	// per value. The link is the first focusable in the page content (after the shell's skip link and
	// navigation) and goes to the list's active row, or to the region itself while there is no list
	// (the empty state, a skeleton, an error).
	function focusResults() {
		const region = document.getElementById('results');
		if (!region) return;
		const row = region.querySelector<HTMLElement>('[role="grid"] [role="row"][tabindex="0"]');
		if (row) return row.focus();
		region.setAttribute('tabindex', '-1');
		region.focus();
	}

	// S-detail's drawer: opened with a shallow pushState carrying `detail` in page.state. It loads
	// on first open, so it never joins this route's first load. (A glob stood here until
	// `DetailHost.svelte` existed, so neither slice edited the other's files.)
	// A failed chunk shows an alert at the top of the results (sticky), without taking focus (the
	// row keeps it). Retry reloads at the drawer's URL, which opens the full page (a reload drops
	// page.state). Back clears it, as it closes the drawer; there is no Close (45 kB cap).
	const drawerOpen = $derived('detail' in page.state);
	let Host = $state<Component>();
	let hostFailed = $state<boolean>();
	$effect(() => {
		if (drawerOpen && !Host) {
			hostFailed = false; // a refetch in flight never shows an earlier failure
			import('$lib/features/detail/DetailHost.svelte').then(
				(mod) => (Host = mod.default),
				() => (hostFailed = true)
			);
		}
	});
</script>

<!-- The page frame (ui/layout), as plain classes: the first view is on a byte budget. -->
<div class={PAGE_FRAME} data-page-frame="">
	<div class={PAGE_BODY_FULL}>
		<!-- The shell's skip link's classes: visible on focus, and no new utilities in the global sheet. -->
		<a
			href="#results"
			class="sr-only rounded-field bg-primary px-4 py-2 text-primary-content focus:not-sr-only focus:fixed focus:start-2 focus:top-2 focus:z-50 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary focus-visible:outline-solid"
			data-testid="skip-to-results"
			onclick={(e) => (e.preventDefault(), focusResults())}>{m.search_page_skip_results()}</a
		>
		<header><h1 class={PAGE_H1} tabindex="-1">{m.search_page_title()}</h1></header>
		<QueryBar bind:this={bar} />
		<div class="search-grid">
			<!-- S-search-2: the facet rail. -->
			<div id="search-rail">
				{#if Facets}<Facets />{/if}
			</div>
			<!-- S-search-3: the results toolbar; S-search-2: the results list. -->
			<div id="results">
				{#if drawerOpen && hostFailed && !Host}
					<div
						class="sticky top-0 z-10 mb-4 flex flex-wrap items-center gap-4 p-6 rounded-box border border-base-300 bg-base-100"
						role="alert"
					>
						{m.search_page_detail_failed()}<button
							class="btn btn-primary"
							onclick={() => location.reload()}>{m.search_page_retry()}</button
						>
					</div>
				{/if}
				{#if Toolbar}<Toolbar />{/if}
				{#if Results}<Results focusBar={() => bar?.focusEnd()} {Home} />{:else if failed}
					<div class="flex flex-col gap-4 p-6" role="alert">
						{m.search_page_results_failed()}<button class="btn btn-primary" onclick={reload}
							>{m.search_page_retry()}</button
						>
					</div>
				{/if}
			</div>
			<!-- S-detail: DetailHost renders here; it decides the drawer's position per breakpoint. -->
			<div id="search-drawer">
				{#if Host}<Host />{/if}
			</div>
		</div>
	</div>
</div>
