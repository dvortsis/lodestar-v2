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
			void import('$lib/keys').then(({ keys }) => {
				if (!gone) off = registerSearchCommands(keys, () => bar?.focusEnd());
			});
		}, KEYS_AFTER_MS);
		return () => {
			gone = true;
			stop();
			off?.();
		};
	});

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
	<h1 class="mt-4 px-4 text-display font-semibold">{m.search_page_title()}</h1>
	<QueryBar bind:this={bar} />
	<div class="search-grid px-4">
		<!-- S-search-2: the facet rail. -->
		<div id="search-rail"></div>
		<!-- S-search-3: the results toolbar; S-search-2: the results list. -->
		<div id="results"></div>
		<!-- S-detail: DetailHost renders here; it decides the drawer's position per breakpoint. -->
		<div id="search-drawer">
			{#if Host && drawerOpen}<Host />{/if}
		</div>
	</div>
</div>
