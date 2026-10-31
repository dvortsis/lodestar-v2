<script lang="ts">
	// The search page's results (§10.4): every state of the table, then the list. Loaded lazily by the
	// page (with a query in the URL, else on the first interaction), never on the bare first load.
	// - The rows follow W5's query: a new parsed query loads; a parse error or a parse in flight keeps
	//   the last good rows, as the strip keeps the last good chips.
	// - j/k move through the rows (scope `search`); `c` and `o` act on the focused row through
	//   S-detail's `setCommandTarget`; Enter or a click opens the drawer.
	import { onMount, tick, untrack } from 'svelte';
	import { page } from '$app/state';
	import { api, type ApiError } from '$lib/api/client';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { shellStatus } from '$lib/shell/status.svelte';
	import type { components } from '$lib/api/schema';
	import { search } from '$lib/state/search.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import VirtualList from '$lib/ui/lists/VirtualList.svelte';
	import { defaultRowHeight, tokenPx } from '$lib/ui/overlays/motion';
	import { setCommandTarget } from '$lib/features/detail/target';
	import {
		chipClauses,
		rootWithout,
		topLevel,
		withoutFilters,
		type Clause
	} from '../query/clauses';
	import { EXAMPLES } from '../query/examples';
	import { ensureSearchPath } from '../query/path';
	import { appearanceHref, searchHref } from './links';
	import { bannerFor, suggestions, type IndexState, type Suggestion } from './recover';
	import { results, type ResultsQuery } from './results.svelte';
	import ResultRow, { type Density, type SearchHit } from './ResultRow.svelte';
	import { selection } from '../bulk/selection.svelte';

	interface Props {
		/** Focus the query bar (the empty state's next step). */
		focusBar: () => void;
	}
	let { focusBar }: Props = $props();

	/** §10.4 "First load": skeletons only after this long, so a quick answer never flashes them. */
	const SKELETON_AFTER_MS = 300;
	/** How long the first rows wait for /search/state (the banner above them). */
	const FIGURES_WAIT_MS = 1000;

	const locale = $derived(currentLocale());
	const density = $derived<Density>(
		page.url.searchParams.get('density') === 'compact' ? 'compact' : 'comfortable'
	);
	const cards = $derived(page.url.searchParams.get('density') === 'cards');

	const tableView = $derived(page.url.searchParams.get('view') === 'table');
	// Table mode, the bulk bar and `x` load on demand (S-search-5): the list's first load is unchanged.
	let Table = $state<typeof import('../table/ResultsTable.svelte').default>();
	$effect(() => {
		if (tableView && !Table)
			void import('../table/ResultsTable.svelte').then((mod) => (Table = mod.default));
	});
	let Bar = $state<typeof import('../bulk/BulkBar.svelte').default>();
	let Pick = $state<typeof import('../bulk/RowPick.svelte').default>();
	$effect(() => {
		if (selection.count > 0 && !Bar)
			void import('../bulk/BulkBar.svelte').then((mod) => (Bar = mod.default));
		if ((selection.mode || selection.count > 0) && !tableView && !Pick)
			void import('../bulk/RowPick.svelte').then((mod) => (Pick = mod.default));
	});
	const hashes = $derived(results.hits.map((h) => h.hash));
	// A new query starts a new selection; a new page (the same query) keeps it.
	$effect(() => selection.follow(results.shown));

	// The query the rows follow. No query in the URL: the empty state. A query that hasn't parsed
	// (in flight, or a syntax error): no new load, so the last good rows stay.
	$effect(() => {
		const q: ResultsQuery = {
			ast: search.q ? search.ast : search.browse ? { op: 'all' } : null,
			sort: search.sort,
			hidden: search.hidden,
			exact: search.exact,
			browse: search.browse
		};
		if (search.q && !search.queryReady) return;
		void results.load(q);
	});

	// A query in the URL that hasn't parsed yet (a pasted link): nothing at all, not the empty state,
	// so the strip's line can arrive above without moving anything (§10.1). A parse error keeps this
	// too; the bar explains it.
	// The index-state banner sits above the rows: they also wait for /search/state (at most
	// FIGURES_WAIT_MS), so it never arrives after them and pushes them down.
	let figuresKnown = $state(false);

	// "Matched in N files": the server sends `matched_files` only for a query with a file clause, so
	// the whole list takes the taller row (one fixed height per list, §10.1).
	const matched = $derived(results.hits.some((h) => h.matched_files !== undefined));
	const rowHeight = $derived(
		density === 'compact'
			? defaultRowHeight()
			: tokenPx('--row-height-comfortable', 64) + (matched ? tokenPx('--spacing', 4) * 5 : 0)
	);

	let slow = $state(false);
	$effect(() => {
		if (results.status !== 'loading') return void (slow = false);
		const t = setTimeout(() => (slow = true), SKELETON_AFTER_MS);
		return () => clearTimeout(t);
	});

	// `/search/state`: fetched once per visit and again on a rebuild event (no polling).
	let figures = $state.raw<IndexState | null>(null);
	let saved = $state.raw<components['schemas']['SavedSearchView'][]>([]);
	// The shell's stream already subscribes to `rebuild` (a second subscriber could reconnect the
	// stream with another topic set): read its snapshot.
	const banner = $derived(bannerFor(figures, shellStatus.rebuild));
	const fullCold = $derived(
		!!figures?.cold_start &&
			(results.status === 'empty' || (results.status === 'ready' && !results.hits.length))
	);
	const suggested = $derived(results.status === 'ready' ? suggestions(results.recovery) : []);
	let Recover = $state<typeof import('./Recover.svelte').default>();
	let Index = $state<typeof import('./IndexState.svelte').default>();
	$effect(() => {
		if (suggested.length && !Recover)
			void import('./Recover.svelte').then((mod) => (Recover = mod.default));
		if ((banner || fullCold) && !Index)
			void import('./IndexState.svelte').then((mod) => (Index = mod.default));
	});
	const waiting = $derived(
		(!!search.q && results.status === 'empty' && search.ast?.op !== 'all') ||
			(results.status !== 'empty' && !figuresKnown) ||
			// the banner's chunk is on its way: the rows wait for it, so it never pushes them down
			((!!banner || fullCold) && !Index)
	);

	async function loadState() {
		try {
			figures = await api.get('/search/state');
		} catch {
			// the page works without it
		} finally {
			figuresKnown = true;
		}
	}
	// The shell's stream carries `rebuild`: a new phase changes the index, so read its state again.
	let seenPhase: string | undefined;
	$effect(() => {
		const phase = shellStatus.rebuild?.phase;
		if (phase === seenPhase) return;
		const first = seenPhase === undefined;
		seenPhase = phase ?? '';
		if (!first || phase) void untrack(loadState);
	});
	async function applySuggestion(item: Suggestion) {
		ensureSearchPath();
		await search.setAst(item.without);
	}

	/** Search's row actions: one lazy chunk, fetched with the first results. */
	let Actions = $state<typeof import('./RowActions.svelte').default>();
	$effect(() => {
		if (results.hits.length && !Actions)
			void import('./RowActions.svelte').then((mod) => (Actions = mod.default));
	});

	// S-search-6: Cards (cards/) and the live banner (live/) load on demand.
	let ext = $state.raw<Partial<Awaited<ReturnType<typeof import('../live/extras').load>>>>({});
	$effect(() => {
		const want = { cards, live: results.status === 'ready' };
		void import('../live/extras')
			.then((mod) => mod.load(want))
			.then((got) => (ext = { Live: got.Live ?? ext.Live, Cards: got.Cards ?? ext.Cards }));
	});

	let active = $state(0);
	let listEl = $state<HTMLElement>();
	type Detail = typeof import('$lib/features/detail/open');
	let detail: Detail | undefined;
	let gone = false;
	const detailMod = () => (detail ? Promise.resolve(detail) : import('$lib/features/detail/open'));

	onMount(() => {
		// Headline figures and the cold-start line; saved searches for the empty state. Failures leave
		// them out: the page works without them.
		const giveUp = setTimeout(() => (figuresKnown = true), FIGURES_WAIT_MS);
		void loadState().finally(() => clearTimeout(giveUp));
		void api.get('/saved-searches').then(
			(s) => (saved = s.slice(0, 5)),
			() => {}
		);
		// `c` and `o` work with the drawer closed: S-detail's commands act on the focused row.
		void import('$lib/features/detail/commands').then((mod) => mod.registerDetailCommands());
		let offKeys: (() => void) | undefined;
		void Promise.all([import('$lib/keys'), import('./commands')]).then(([{ keys }, cmd]) => {
			if (gone) return;
			offKeys = cmd.registerResultsCommands(keys, {
				next: () => move(1),
				previous: () => move(-1),
				select: () => {
					const hit = results.hits[active];
					if (hit) selection.toggle(hit.hash);
				}
			});
		});
		return () => {
			clearTimeout(giveUp);
			results.forget(); // answers from before this mount may be stale when it comes back
			gone = true;
			detail?.cancelDetailPrefetch();
			offKeys?.();
			setCommandTarget(null);
		};
	});

	/** j/k: the next or previous row becomes active (and takes focus unless the drawer is open). */
	let list = $state<{ move: (delta: number) => void; moveTo: (index: number) => void }>();
	let grid = $state<{ move: (delta: number) => void; moveTo: (index: number) => void }>();
	const nav = () => (tableView ? grid : list);

	// The sentinel's fallback for the keyboard and AT: a visible "Load more". Focus stays on it while
	// a page loads (`loading` keeps it focusable). When it goes (the last page, an error) or comes
	// back (Retry), focus moves to what replaced it: checked before the DOM changes, while the old
	// control still has it.
	let foot = $state<HTMLElement>();
	$effect.pre(() => {
		void results.hasMore;
		if (!foot?.contains(document.activeElement)) return;
		void tick().then(() =>
			foot
				?.querySelector<HTMLElement>(
					'[data-results-more], [data-results-end], [data-results-retry]'
				)
				?.focus()
		);
	});
	function move(by: 1 | -1) {
		const n = results.hits.length;
		if (!n) return;
		if ('detail' in page.state) {
			// §10.5: with the drawer open, the drawer follows and focus stays in it (so Esc still
			// closes it and goes back to the row).
			active = Math.min(n - 1, Math.max(0, active + by));
			const hit = results.hits[active];
			if (hit) void open(hit);
			return;
		}
		// From outside the list, j/k go to the active row first (row 1 on a fresh list).
		if (listEl?.contains(document.activeElement)) nav()?.move(by);
		else nav()?.moveTo(active);
	}

	/**
	 * The row with focus, however it got it (Tab, a click, the arrows, j/k): `c`/`o` act on it, and
	 * its details are fetched ahead (150 ms idle). Focus leaving the list clears the target.
	 */
	function focusIn(event: FocusEvent) {
		const hash = hashOf(event.target);
		if (!hash) return;
		setCommandTarget(hash); // synchronous: a `c` right after the focus moves must see it
		void detailMod().then((mod) => {
			if (gone) return;
			detail = mod;
			mod.prefetchDetail(hash);
		});
	}
	/** The hash of the row (a list row or a table row) holding `target`. */
	function hashOf(target: EventTarget | null) {
		// A Cards tile carries its own hash (a list row holds several).
		const el = target as HTMLElement;
		return (
			el.closest<HTMLElement>('[data-hash]') ??
			el.closest('[role="row"], tr')?.querySelector<HTMLElement>('[data-hash]')
		)?.dataset.hash;
	}
	function focusOut(event: FocusEvent) {
		if (listEl?.contains(event.relatedTarget as Node | null)) return;
		setCommandTarget(null);
	}

	async function open(hit: SearchHit) {
		const mod = await detailMod();
		if (gone) return;
		detail = mod;
		const el = listEl?.querySelector<HTMLElement>(`[data-hash="${CSS.escape(hit.hash)}"]`);
		const rowEl = cards ? el : el?.closest<HTMLElement>('[role="row"], tr');
		mod.openDetail(hit.hash, {
			hit,
			returnFocus: () => rowEl ?? null,
			replace: 'detail' in page.state
		});
	}

	/** §10.14: what happened, then what to do. */
	function errorText(e: ApiError): string {
		if (e.status === 429 || e.status === 503)
			return e.retryAfter
				? m.search_results_error_busy({ seconds: formatCount(e.retryAfter, locale) })
				: m.search_results_error_busy_now();
		if (e.status >= 400 && e.status < 500) return m.search_results_error_query();
		return m.search_results_error_body();
	}

	const skeletons = Array.from({ length: 8 }, (_, i) => i);

	// §10.4 "No results": the query's filters, each removable, and Clear all (one history entry).
	const clauses = $derived(results.status === 'ready' ? chipClauses(search.ast) : []);
	async function removeClause(clause: Clause) {
		const base = search.ast;
		const index = topLevel(base).indexOf(clause.node);
		if (!base || index < 0) return;
		ensureSearchPath();
		await search.setAst(rootWithout(base, index));
	}
	async function clearFilters() {
		ensureSearchPath();
		if (search.browse) return search.reset();
		await search.setAst(withoutFilters(search.ast), search.hidden ? { hidden: false } : {});
	}

	// §10.13: the count, politely, once per answered query (W5 debounces typing): the effect
	// follows only the query the rows answer, not the rows (a new page isn't a new count). Cleared
	// first so the same count is read again for a new query.
	let said = $state('');
	$effect(() => {
		const shown = results.shown;
		if (shown === null) return;
		const text = untrack(announcement);
		if (text === null) return;
		said = '';
		const raf = requestAnimationFrame(() => (said = text));
		return () => cancelAnimationFrame(raf);
	});
	function announcement(): string | null {
		if (results.status !== 'ready') return null;
		const n = results.hits.length === 0 ? 0 : results.total;
		const count = formatCount(n, locale);
		return n === 0
			? m.search_results_none()
			: results.totalApprox
				? m.search_results_announce_about({ count })
				: n === 1
					? m.search_results_announce_one()
					: m.search_results_announce_other({ count });
	}

	// §10.13: the selection count, politely, through the same region as the result count.
	let announced = selection.count;
	$effect(() => {
		const n = selection.count;
		if (n === announced) return;
		announced = n;
		void import('../bulk/ops').then(
			(ops) => (said = ops.countText(n, (c) => formatCount(c, locale)))
		);
	});

	/** §10.1: details are fetched ahead on hover too (the 150 ms idle is `prefetchDetail`'s). */
	let hovered: string | undefined;
	function pointerOver(event: PointerEvent) {
		const hash = hashOf(event.target);
		if (!hash || hash === hovered) return;
		hovered = hash;
		void detailMod().then((mod) => {
			if (gone) return;
			detail = mod;
			mod.prefetchDetail(hash);
		});
	}

	/** Shift+F10 or the menu key opens the focused row's More menu: its actions without a pointer. */
	function keyDown(event: KeyboardEvent) {
		if (!(event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10'))) return;
		const more = (event.target as HTMLElement)
			.closest('[role="row"], tr')
			?.querySelector<HTMLButtonElement>('[data-row-more]');
		if (!more) return;
		event.preventDefault();
		more.click();
	}
</script>

<section
	class="rp"
	aria-labelledby="rp-label"
	aria-busy={results.status === 'loading' || results.refining}
>
	<h2 id="rp-label" class="sr-only">{m.search_results_label()}</h2>
	{#if results.refining}<div class="rp-hairline" data-testid="results-refining"></div>{/if}

	<p class="sr-only" aria-live="polite" data-testid="results-announce">{said}</p>

	{#if Index && figures}<Index {banner} state={figures} full={fullCold} />{/if}

	{#if ext.Live}<ext.Live bind:said list={nav} />{/if}

	{#if waiting}
		<!-- the parse is out: see `waiting` -->
	{:else if fullCold && Index && results.status === 'empty'}
		<!-- the cold-start page above stands in for the empty state -->
	{:else if results.status === 'empty'}
		<div class="rp-empty" data-testid="results-empty">
			<p class="rp-empty-title" data-testid="results-empty-title">
				{figures
					? m.search_results_empty_title({
							count: formatCount(figures.searchable_torrents, locale)
						})
					: m.search_results_empty_title_unknown()}
			</p>
			<p>{m.search_results_empty_lead()}</p>
			<ul class="rp-examples">
				{#each EXAMPLES.slice(0, 3) as ex (ex.query)}
					<li>
						<!-- The query last on its line: the mono font arrives late, and growing text there moves
						     nothing after it (a layout shift otherwise). -->
						{ex.goal()} ·
						<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- searchHref() resolves -->
						<a class="rp-link" href={searchHref(ex.query)}><code>{ex.query}</code></a>
					</li>
				{/each}
			</ul>
			<Button onclick={focusBar}>{m.search_results_empty_focus()}</Button>
			<!-- Last: the list arrives with its request, after the rest has painted, and must not push
			     anything down (a layout shift). -->
			{#if saved.length}
				<h3 class="rp-empty-sub">{m.search_results_empty_saved()}</h3>
				<ul class="rp-examples" data-testid="results-saved">
					{#each saved as s (s.id)}
						<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- searchHref() resolves -->
						<li><a class="rp-link" href={searchHref(s.q, s.sort)}>{s.name}</a></li>
					{/each}
				</ul>
			{/if}
		</div>
	{:else if results.status === 'loading'}
		<div class="rp-skeleton" data-testid="results-loading" role="status">
			<span class="sr-only">{m.search_results_loading()}</span>
			{#if slow}
				<!-- §10.4: at the rows' final height, so nothing moves when they arrive -->
				{#each skeletons as n (n)}<div class="rp-skeleton-cell" style:height="{rowHeight}px">
						<Skeleton class="rp-skeleton-row" />
					</div>{/each}
			{/if}
		</div>
	{:else if results.status === 'error' && results.error}
		<Banner kind="error" title={m.search_results_error()} class="rp-banner">
			<span data-testid="results-error">{errorText(results.error)}</span>
			{#snippet action()}
				<Button dense onclick={() => void results.retry()}>{m.search_query_retry()}</Button>
			{/snippet}
		</Banner>
	{:else}
		{#if results.error}
			<Banner kind="error" title={m.search_results_error()} class="rp-banner">
				<span data-testid="results-error">{errorText(results.error)}</span>
				{#snippet action()}
					<Button dense onclick={() => void results.retry()}>{m.search_query_retry()}</Button>
				{/snippet}
			</Banner>
		{/if}
		{#if results.expired}
			<Banner kind="info" class="rp-banner">
				<span data-testid="results-expired">{m.search_results_cursor_expired()}</span>
			</Banner>
		{/if}
		{#if results.notices.includes('adult_hidden')}
			<Banner kind="info" title={m.search_results_adult_hidden()} class="rp-banner">
				<span data-testid="results-adult-hidden">{m.search_results_adult_hidden_body()}</span>
				{#snippet action()}
					<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- appearanceHref() resolves -->
					<a class="rp-link" href={appearanceHref()}>{m.search_results_adult_settings()}</a>
				{/snippet}
			</Banner>
		{/if}
		{#if results.hits.length === 0}
			<div class="rp-none" data-testid="results-none">
				<p class="rp-empty-title">{m.search_results_none()}</p>
				{#if clauses.length}
					<p>{m.search_results_none_lead()}</p>
					{#if Recover && suggested.length}<Recover
							items={suggested}
							apply={(i) => void applySuggestion(i)}
						/>{/if}
					<ul class="rp-examples" data-testid="results-none-clauses">
						{#each clauses as clause, i (i)}
							<li>
								<Button dense onclick={() => void removeClause(clause)}
									>{m.search_results_none_remove({ label: clause.label.text })}</Button
								>
							</li>
						{/each}
					</ul>
					<Button onclick={() => void clearFilters()}>{m.search_results_none_clear()}</Button>
				{:else if !results.notices.includes('adult_hidden')}
					<p>{m.search_results_none_words()}</p>
				{/if}
			</div>
		{:else}
			<!-- Delegated handlers for the grid's rows (focus, hover prefetch, Shift+F10): the rows are
			     the interactive elements, this wrapper only listens. -->
			<!-- svelte-ignore a11y_no_static_element_interactions -->
			<div
				class:rp-list={!tableView}
				bind:this={listEl}
				onfocusin={focusIn}
				onfocusout={focusOut}
				onpointerover={pointerOver}
				onkeydown={keyDown}
			>
				{#if tableView}
					{#if Table}
						<Table
							bind:this={grid}
							hits={results.hits}
							hasMore={results.hasMore}
							onLoadMore={() => void results.loadMore()}
							onOpen={(h: SearchHit) => void open(h)}
							bind:active
						/>
					{/if}
				{:else if cards}
					{#if ext.Cards}<ext.Cards bind:this={list} bind:active open={(h) => void open(h)} />{/if}
				{:else}
					<VirtualList
						bind:this={list}
						items={results.hits}
						role="grid"
						getKey={(h) => h.hash}
						label={m.search_results_label()}
						estimateSize={rowHeight}
						resetKey={results.shown ?? ''}
						bind:activeIndex={active}
						onActivate={(_i, hit) => void open(hit)}
						hasMore={results.hasMore}
						loadingMore={results.loadingMore}
						onLoadMore={() => void results.loadMore()}
						loadMoreLabel={m.search_results_loading_more()}
					>
						{#snippet row(hit)}
							<div role="gridcell" class="rp-cell" data-hash={hit.hash}>
								{#if Pick && (selection.mode || selection.count > 0)}
									<Pick {hit} rows={hashes} />
								{/if}
								<ResultRow {hit} {density} {matched} testid="result-{hit.hash}">
									{#snippet actions(h)}
										{#if Actions}<Actions hit={h} {density} testid="result-{h.hash}" />{/if}
									{/snippet}
								</ResultRow>
							</div>
						{/snippet}
					</VirtualList>
				{/if}
			</div>
			<div bind:this={foot}>
				<div class="rp-foot">
					{#if results.hasMore}
						<Button
							dense
							data-results-more
							loading={results.loadingMore}
							onclick={() => void results.loadMore()}>{m.search_results_load_more()}</Button
						>
					{:else if !results.moreError}
						<p class="rp-end" tabindex="-1" data-results-end data-testid="results-end">
							{m.search_results_end()}
						</p>
					{/if}
				</div>
				{#if results.moreError}
					<Banner kind="error" class="rp-banner">
						{m.search_results_more_error()}
						{#snippet action()}
							<Button dense data-results-retry onclick={() => void results.retry()}
								>{m.search_query_retry()}</Button
							>
						{/snippet}
					</Banner>
				{/if}
			</div>
			{#if Bar && selection.count > 0}<Bar />{/if}
		{/if}
	{/if}
</section>
