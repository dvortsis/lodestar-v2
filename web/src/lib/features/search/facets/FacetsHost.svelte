<script lang="ts">
	// Where the facet rail lives (§10.15), loaded with the results:
	// - x-large and up: the 264 px rail, in flow (`#search-rail`);
	// - large: a "Filters (N)" button opening a left sheet; a click applies at once;
	// - medium and small: a bottom sheet that stages its clicks. "Show N results" counts the staged
	//   query (`/facets`'s `total`), and nothing is written until it is pressed.
	// The rail's chunk, its counts and the sheet load only where the rail shows: below x-large, on
	// the first open.
	import { onMount } from 'svelte';
	import Sliders from '~icons/ph/sliders-horizontal';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { search, type Ast } from '$lib/state/search.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { chipClauses } from '../query/clauses';
	import { ensureSearchPath } from '../query/path';
	import { results } from '../results/results.svelte';
	import { createFacets, MORE } from './facets.svelte';

	type Mode = 'rail' | 'left' | 'bottom';
	// design-tokens.md §7: x-large 1024 px, large 640 px.
	const RAIL = '(min-width: 1024px)';
	const LEFT = '(min-width: 640px)';
	const modeNow = (): Mode =>
		typeof matchMedia !== 'function' || matchMedia(RAIL).matches
			? 'rail'
			: matchMedia(LEFT).matches
				? 'left'
				: 'bottom';

	// Read at once, so a narrow first render never builds the rail.
	let mode = $state<Mode>(modeNow());
	onMount(() => {
		if (typeof matchMedia !== 'function') return;
		const rail = matchMedia(RAIL);
		const left = matchMedia(LEFT);
		const update = () => {
			const next = modeNow();
			if (next !== mode) open = false; // a sheet doesn't follow the page across breakpoints
			mode = next;
		};
		rail.addEventListener('change', update);
		left.addEventListener('change', update);
		return () => {
			rail.removeEventListener('change', update);
			left.removeEventListener('change', update);
		};
	});

	const locale = $derived(currentLocale());
	let open = $state(false);
	const shown = $derived(
		results.shown === null ? null : (JSON.parse(results.shown) as [Ast, string, boolean, boolean])
	);
	const wanted = $derived(shown !== null && (mode === 'rail' || open));

	let Rail = $state<typeof import('./FacetRail.svelte').default>();
	let Sheet = $state<typeof import('$lib/ui/overlays/Sheet.svelte').default>();
	$effect(() => {
		if (wanted && !Rail) void import('./FacetRail.svelte').then((mod) => (Rail = mod.default));
	});

	// "Show more" per facet (up to 100 values), kept across queries.
	let more = $state.raw<ReadonlySet<string>>(new Set());
	const limits = $derived(Object.fromEntries([...more].map((name) => [name, MORE])));
	function setMore(name: string, on: boolean) {
		more = new Set(on ? [...more, name] : [...more].filter((n) => n !== name));
	}

	// The counts for the rows shown, on their snapshot.
	const live = createFacets();
	$effect(() => {
		if (!wanted || !shown) return;
		const [ast, sort, hidden] = shown;
		void live.load({ ast, sort, hidden, lease: results.lease, limits });
	});

	// What a click edits: the newest query, never the rows' (`shown` changes only when /search
	// answers, so two quick clicks, or a click after a new query failed, would edit a stale one).
	// The edit handed to W5 counts until W5 adopts it (its `ast` changes once the server printed it
	// back), then W5's parsed query; the rows' query only while W5 has none (a parse error).
	let pending = $state.raw<Ast | null>(null);
	$effect(() => {
		void search.ast;
		pending = null;
	});
	const base = $derived(pending ?? search.ast ?? shown?.[0] ?? null);

	// The bottom sheet's staged query and its own counts (no lease: it isn't a shown query).
	let staged = $state.raw<Ast | null>(null);
	const preview = createFacets();
	// Staging only once the staged query differs from the shown one: until then the live counts
	// are the right ones, and no second request is needed.
	const staging = $derived(
		mode === 'bottom' &&
			open &&
			staged !== null &&
			JSON.stringify(staged) !== JSON.stringify(shown?.[0] ?? null)
	);
	$effect(() => {
		if (!staging || !shown) return;
		const [, sort, hidden] = shown;
		void preview.load({ ast: staged, sort, hidden, lease: null, limits });
	});
	const view = $derived(staging ? (preview.data ?? live.data) : live.data);
	const viewAst = $derived(staging ? staged : base);

	function apply(next: Ast) {
		ensureSearchPath();
		pending = next;
		search.setAst(next).catch(() => {
			if (pending === next) pending = null; // W5 kept its query: edit that one
		});
	}
	function change(next: Ast) {
		if (mode === 'bottom') staged = next;
		else apply(next);
	}
	async function openSheet() {
		staged = base;
		void preview.load({ ast: null, sort: '', hidden: false, lease: null }); // forget the last session's
		Sheet ??= (await import('$lib/ui/overlays/Sheet.svelte')).default;
		open = true;
	}
	function show() {
		if (staged) apply(staged);
		open = false;
	}

	const count = $derived(chipClauses(search.ast).length);
	const showLabel = $derived.by(() => {
		const d = staging ? preview.data : live.data;
		if (!d || (staging && preview.loading)) return m.search_facets_show_results({ count: '…' });
		return d.total === 1 && !d.total_approx
			? m.search_facets_show_results_one()
			: m.search_facets_show_results({
					count: formatCount({ value: d.total, approx: d.total_approx }, locale)
				});
	});
	// §10.13: a staged click's only feedback is this count, so it is said politely once it settles.
	const said = $derived(staging && !preview.loading && preview.data ? showLabel : '');
</script>

{#snippet rail()}
	{#if Rail}
		<Rail
			data={view}
			ast={viewAst}
			error={staging ? preview.error : live.error}
			{more}
			onmore={setMore}
			onchange={change}
			onretry={() => void (staging ? preview.retry() : live.retry())}
		/>
	{/if}
{/snippet}

<!-- A pasted link shows the Filters button from the start, before the rows: they can't move. -->
{#if shown || (mode !== 'rail' && search.q)}
	{#if mode === 'rail'}
		<aside aria-labelledby="fr-heading" data-testid="facet-host">
			<h2 id="fr-heading" class="sr-only">{m.search_facets_label()}</h2>
			{@render rail()}
		</aside>
	{:else}
		<div data-testid="facet-host">
			<Button onclick={() => void openSheet()} data-testid="facet-open">
				<Sliders class="size-(--icon-size-row)" aria-hidden="true" />
				{count
					? m.search_facets_button({ count: formatCount(count, locale) })
					: m.search_facets_button_none()}
			</Button>
			{#if Sheet}
				<Sheet
					bind:open
					side={mode === 'left' ? 'left' : 'bottom'}
					title={m.search_facets_label()}
					closeLabel={m.search_facets_close()}
				>
					{@render rail()}
					<p class="sr-only" aria-live="polite">{said}</p>
					{#snippet footer()}
						{#if mode === 'bottom'}
							<Button variant="primary" onclick={show} data-testid="facet-show">{showLabel}</Button>
						{/if}
					{/snippet}
				</Sheet>
			{/if}
		</div>
	{/if}
{/if}
