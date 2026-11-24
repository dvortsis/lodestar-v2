<script lang="ts">
	// The search page's results (§10.4): every state of the table, then the list. Loaded lazily by the
	// page (with a query in the URL, else on the first interaction), never on the bare first load.
	// - The rows follow W5's query: a new parsed query loads; a parse error or a parse in flight keeps
	//   the last good rows, as the strip keeps the last good chips.
	// - j/k move through the rows (scope `search`); `c` and `o` act on the focused row through
	//   S-detail's `setCommandTarget`; Enter or a click opens the drawer.
	import { onMount, tick, untrack, type Component } from 'svelte';
	import { page } from '$app/state';
	import { api, type ApiError } from '$lib/api/client';
	import type { components } from '$lib/api/schema';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { shellStatus } from '$lib/shell/status.svelte';
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
	import { ensureSearchPath } from '../query/path';
	import { appearanceHref } from './links';
	import { forget, recall, remember } from './index-memory';
	import { bannerFor, type Banner as Kind, type IndexState } from './recover';
	import { results, type ResultsQuery } from './results.svelte';
	import ResultRow, { type Density, type SearchHit } from './ResultRow.svelte';
	import { selection } from '../bulk/selection.svelte';
	import { groupedIn, grouping } from '../collapse/grouping.svelte';
	import Others from '../collapse/Others.svelte';
	import { groupsCount } from '../collapse/count';
	import type { Expand } from '../collapse/expand.svelte';

	interface Props {
		/** Focus the query bar (the empty state's next step). */
		focusBar: () => void;
		/** The home screen (search/home), loaded beside the panel in its own chunk. */
		Home?: Component<{
			focusBar: () => void;
			count: number | null;
			saved: components['schemas']['SavedSearchView'][];
		}>;
	}
	let { focusBar, Home }: Props = $props();

	/** §10.4 "First load": skeletons only after this long, so a quick answer never flashes them. */
	const SKELETON_AFTER_MS = 300;
	/** How long the first rows wait for /search/state (the banner above them). */
	const FIGURES_WAIT_MS = 1000;

	const locale = $derived(currentLocale());
	// The view's two URL parameters, else the account's Appearance density (toolbar/view.ts has the same
	// rule, for the toolbar: change both together). Read from the query string, so a late preference
	// takes effect with the next query, never when the drawer closes or the list is on screen.
	const qs = $derived(page.url.search);
	const look = $derived(
		new URLSearchParams(qs).get('density') ?? document.documentElement.dataset.density
	);
	const density = $derived<Density>(look === 'compact' ? 'compact' : 'comfortable');
	const cards = $derived(look === 'cards');

	const tableView = $derived(new URLSearchParams(qs).get('view') === 'table');
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

	// GS-5: a grouped row shows "+N other versions" (Others, with the panel); opening a group loads
	// the rest (collapse/rows.ts) and adds its rows under the best version. Until a group is first
	// opened, and whenever none is open, the rows are the hits.
	let G = $state.raw<typeof import('../collapse/rows')>();
	let expand = $state.raw<Expand>();
	let expanding: Promise<Expand | undefined> | undefined;
	/** Runs `run` once the group part is in; a press while it loads waits for the same load. */
	function withExpand(run: (x: Expand) => void) {
		if (expand) return run(expand);
		const shown = results.shown;
		expanding ??= import('../collapse/rows').then(
			(mod) => {
				G = mod;
				expand = mod.createExpand({ onsettled: say });
				expand.follow(results.shown);
				return expand;
			},
			() => ((expanding = undefined), undefined) // offline: the next press tries again
		);
		void expanding.then((x) => {
			// Pressed for rows a newer query has replaced: nothing to open.
			if (x && results.shown === shown) run(x);
		});
	}
	$effect(() => expand?.follow(results.shown));
	// Groups open in the List only; Table and Cards show one row per group and a Versions count.
	const rows = $derived(expand && !tableView && !cards ? expand.rows(results.hits) : results.hits);
	const hitAt = (i: number) => {
		const r = rows[i];
		return r && G ? G.hitOf(r) : (r as SearchHit | undefined);
	};
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
			browse: search.browse,
			collapse: false
		};
		// The account's choice as this browser knew it when this search started: a late answer, or a
		// change of view, never re-runs the rows on screen (grouping.svelte.ts `pin`).
		const key = JSON.stringify([q.ast, q.sort, q.hidden, q.exact, q.browse]);
		q.collapse = groupedIn(
			qs,
			untrack(() => grouping.pin(key))
		);
		if (search.q && !search.queryReady) return;
		void results.load(q);
	});

	// A query in the URL that hasn't parsed yet (a pasted link): nothing at all, not the empty state,
	// so the strip's line can arrive above without moving anything (§10.1). A parse error keeps this
	// too; the bar explains it.
	// The index-state banner sits above the rows: they also wait for /search/state (at most
	// FIGURES_WAIT_MS), and past that paint with the last answer this browser saw (`growingShown`).
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
	const live = $derived(bannerFor(figures, shellStatus.rebuild));
	// X-index-banner-late: "still growing" shows or goes only while no rows are on screen, so it never
	// moves a painted row. The rows paint with the live answer, else the remembered one; a later answer
	// that disagrees waits for the next moment without rows (an empty, loading or error state, or the
	// next visit), and so do its figures. A rebuild stays live: see the decisions file.
	// Read once per mount, deliberately: later answers come through `figures`. A held banner whose
	// live answer says "not growing" (or failed) keeps the remembered figures on its line.
	const memory = recall();
	// A failed /search/state drops a remembered banner at the next moment without rows, as on main.
	// The memory applies only once the wait is over, so no banner shows over the blank wait that a
	// fast live answer could then take away.
	let stateFailed = $state(false);
	const want = $derived(
		figures ? figures.growing : figuresKnown && !stateFailed && (memory?.growing ?? false)
	);
	let growingShown = $state(false);
	const banner = $derived<Kind>(
		live?.kind === 'rebuild' ? live : growingShown ? { kind: 'growing' } : null
	);
	const fullCold = $derived(figures?.cold_start && results.status === 'empty');
	const suggested = $derived(results.recovery?.clauses.some((c) => c.total));
	// A failed chunk leaves No results as it was: the filters and Clear all, without the counts.
	let recoverFailed = $state(false);
	let Recover = $state<typeof import('./Recover.svelte').default>();
	let Index = $state<typeof import('./IndexState.svelte').default>();
	let indexFailed = $state(false);
	$effect(() => {
		if (suggested && !Recover && !recoverFailed)
			void import('./Recover.svelte').then(
				(mod) => (Recover = mod.default),
				() => (recoverFailed = true)
			);
		if ((banner || fullCold) && !Index) void ensureIndex();
	});
	/** The banner chunk; a failed load leaves the page without the banner, never without rows. */
	const ensureIndex = () =>
		import('./IndexState.svelte').then(
			(mod) => (Index = mod.default),
			() => (indexFailed = true)
		);
	// The rows also wait for the banner's chunk when it's to show (`growingShown` only changes without
	// rows, so this never takes painted rows away).
	const waiting = $derived(
		(!!search.q && results.status === 'empty' && search.ast?.op !== 'all') ||
			!figuresKnown ||
			(growingShown && !Index && !indexFailed)
	);
	const rowsUp = $derived(!waiting && results.status === 'ready' && results.hits.length > 0);
	let painted = false; // rows were on screen at the last DOM update
	$effect.pre(() => {
		// A failed chunk keeps the banner off until a moment without rows: a retry that lands later
		// would otherwise add it over painted rows.
		const next = want && (!!Index || !indexFailed);
		if (!rowsUp || !painted) growingShown = next;
	});
	$effect(() => {
		painted = rowsUp;
	});

	async function loadState() {
		try {
			figures = await api.get('/search/state');
			remember(figures);
			// The rows wait for this (at most FIGURES_WAIT_MS), so the banner never pushes them down.
			if (bannerFor(figures, shellStatus.rebuild) || figures.cold_start) await ensureIndex();
		} catch {
			// the page works without it; a stale memory lasts at most this visit
			stateFailed = true;
			forget();
		} finally {
			figuresKnown = true;
		}
	}
	// The shell's stream carries `rebuild`: a new phase changes the index, so read its state again.
	// The phase at mount is not a change: onMount already fetches once per visit.
	let seenPhase = shellStatus.rebuild?.phase;
	$effect(() => {
		const phase = shellStatus.rebuild?.phase;
		if (phase === seenPhase) return;
		seenPhase = phase;
		void untrack(loadState);
	});

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
	// The active row follows its torrent across a change of view: the List counts an open group's
	// rows, the Table and Cards one row per group, so the same index would land on another torrent.
	// A version (or a group's line) that the new view doesn't show hands over to its group's row.
	const view = $derived(tableView ? 'table' : cards ? 'cards' : 'list');
	let viewWas = untrack(() => view);
	let rowsWas: typeof rows = [];
	/** The row carried into the Table, set again once it mounts (it starts each mount at row 1). */
	let carried: number | null = null;
	$effect.pre(() => {
		const now = view;
		const next = rows;
		untrack(() => {
			const was = rowsWas[active];
			if (now !== viewWas) carried = null;
			if (now !== viewWas && was && G) {
				const find = (hash: string) =>
					next.findIndex((r) => !G!.isGroupRow(r) && (r as SearchHit).hash === hash);
				let i = find(G.hitOf(was).hash);
				if (i < 0 && G.isGroupRow(was)) i = find(was.parent.hash);
				if (i >= 0) active = i;
				if (i >= 0 && now === 'table') carried = i;
			}
			viewWas = now;
			rowsWas = next;
		});
	});
	$effect(() => {
		if (!grid || carried === null) return;
		active = carried;
		carried = null;
	});
	let listEl = $state<HTMLElement>();
	type Detail = typeof import('$lib/features/detail/open');
	let detail: Detail | undefined;
	let gone = false;
	// True once j/k/x and c/o are registered (they load lazily): a key pressed before that is dropped, so the
	// e2e specs wait on `data-keys-ready` before pressing a list key.
	let keysReady = $state(false);
	const detailMod = () => (detail ? Promise.resolve(detail) : import('$lib/features/detail/open'));

	onMount(() => {
		// Headline figures and the cold-start line; saved searches for the empty state. Failures leave
		// them out: the page works without them.
		const giveUp = setTimeout(() => (figuresKnown = true), FIGURES_WAIT_MS);
		if (memory?.growing) void ensureIndex(); // ready for the rows' first frame if the wait runs out
		void loadState().finally(() => clearTimeout(giveUp));
		void api.get('/saved-searches').then(
			(s) => (saved = s.slice(0, 5)),
			() => {}
		);
		// `c` and `o` work with the drawer closed: S-detail's commands act on the focused row.
		const detailKeys = import('$lib/features/detail/commands').then(
			(mod) => mod.registerDetailCommands(),
			() => {}
		);
		let offKeys: (() => void) | undefined;
		void Promise.all([import('$lib/keys'), import('./commands')]).then(([{ keys }, cmd]) => {
			if (gone) return;
			offKeys = cmd.registerResultsCommands(keys, {
				next: () => move(1),
				previous: () => move(-1),
				select: () => {
					const r = rows[active];
					if (r && !G?.isGroupRow(r)) selection.toggle((r as SearchHit).hash);
				}
			});
			// c/o come from the detail chain; ready means every list key is in.
			void detailKeys.then(() => {
				if (!gone) keysReady = true;
			});
		});
		return () => {
			clearTimeout(giveUp);
			results.forget(); // answers from before this mount may be stale when it comes back
			gone = true;
			cancelAnimationFrame(saying);
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
		const n = rows.length;
		if (!n) return;
		if ('detail' in page.state) {
			// §10.5: with the drawer open, the drawer follows and focus stays in it (so Esc still
			// closes it and goes back to the row).
			// A group's Show all, loading, failed or empty line has no torrent of its own: step over it.
			let i = active + by;
			while (i >= 0 && i < n && isStateRow(rows[i]!)) i += by;
			if (i < 0 || i >= n) return; // nothing past the line at either end: stay put
			active = i;
			const hit = hitAt(active);
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

	async function open(hit: SearchHit, section?: 'related') {
		const mod = await detailMod();
		if (gone) return;
		detail = mod;
		const el = listEl?.querySelector<HTMLElement>(`[data-hash="${CSS.escape(hit.hash)}"]`);
		const rowEl = cards ? el : el?.closest<HTMLElement>('[role="row"], tr');
		mod.openDetail(hit.hash, {
			hit,
			returnFocus: () => rowEl ?? null,
			replace: 'detail' in page.state,
			section
		});
	}

	const isStateRow = (r: (typeof rows)[number]) => 'row' in r && r.row !== 'version';

	/** A grouped List is a tree grid: a row with versions says whether they're open; theirs are level 2. */
	function treeAttrs(r: (typeof rows)[number]) {
		if ('row' in r) return { 'aria-level': 2 };
		const hit = r as SearchHit;
		return {
			'aria-level': 1,
			'aria-expanded': hit.others ? (expand?.isOpen(hit.hash) ?? false) : undefined
		};
	}

	/** Enter or a click on a row: a torrent opens its drawer; a group's own rows act for it (GS-5). */
	function activate(r: (typeof rows)[number]) {
		if (!G || !G.isGroupRow(r)) return void open(r as SearchHit);
		if (r.row === 'failed') expand?.expand(r.parent);
		else if (r.row === 'version') void open(r.hit);
		// Show all: the best version's details at Related, which lists every other version.
		else if (r.row === 'more') void open(r.parent, 'related');
	}
	/** → opens a group, ← closes it; from a version row, focus goes back to the best row. */
	function groupKey(event: KeyboardEvent, ctx: { item: (typeof rows)[number]; onRow: boolean }) {
		if (!ctx.onRow) return;
		if (!expand) {
			// Nothing opened yet, so every row is a torrent: → loads the group's part and opens it.
			const hit = ctx.item as SearchHit;
			const plain = !(event.altKey || event.ctrlKey || event.metaKey || event.shiftKey);
			if (event.key === 'ArrowRight' && plain && hit.others) {
				event.preventDefault();
				withExpand((x) => x.expand(hit));
			}
			return;
		}
		const back = expand.key(event, ctx.item);
		if (back !== undefined)
			void tick().then(() => list?.moveTo(rows.findIndex((x) => G?.keyOf(x) === back)));
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
	let saying = 0;
	/** Says `text` politely, cleared first so the same words (a second group's load) are read again. */
	function say(text: string) {
		said = '';
		cancelAnimationFrame(saying);
		saying = requestAnimationFrame(() => (said = text));
	}
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
		const g = results.grouped;
		if (g.collapsed && g.groups !== undefined && n > 0)
			return groupsCount(g.groups, !!g.groups_approx, n, results.totalApprox, locale);
		const count = formatCount(n, locale);
		return n === 0
			? m.search_results_none()
			: results.totalApprox
				? m.search_results_announce_about({ count })
				: n === 1
					? m.search_results_announce_one()
					: m.search_results_announce_other({ count });
	}

	// A Block that dropped rows says so through the same region (the store's note, read once).
	$effect(() => {
		const note = results.note;
		if (!note) return;
		cancelAnimationFrame(saying); // a group's load still waiting for its frame doesn't cover it
		said = note;
		results.note = '';
	});

	// §10.13: the selection count, politely, through the same region as the result count.
	let announced = selection.count;
	$effect(() => {
		const n = selection.count;
		if (n === announced) return;
		announced = n;
		void import('../bulk/ops').then((ops) => {
			cancelAnimationFrame(saying);
			said = ops.countText(n, (c) => formatCount(c, locale));
		});
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
	<h2 id="rp-label" class="sr-only" tabindex="-1">{m.search_results_label()}</h2>
	{#if results.refining}<div class="rp-hairline" data-testid="results-refining"></div>{/if}

	<p class="sr-only" aria-live="polite" data-testid="results-announce">{said}</p>

	{#if Index && (banner || fullCold)}<Index
			{banner}
			state={figures?.growing || !memory ? figures : memory}
			hold={rowsUp}
			full={fullCold}
			{focusBar}
		/>{/if}

	{#if ext.Live}<ext.Live bind:said list={nav} />{/if}

	{#if waiting}
		<!-- the parse is out: see `waiting` -->
	{:else if fullCold && Index}
		<!-- the cold-start page above stands in for the empty state -->
	{:else if results.status === 'empty'}
		{#if Home}<Home {focusBar} count={figures?.searchable_torrents ?? null} {saved} />{/if}
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
			<!-- the counts' chunk is on its way: the block waits for it, so the buttons never move -->
			<div
				class="rp-none"
				data-testid="results-none"
				hidden={suggested && !Recover && !recoverFailed}
			>
				<p class="rp-empty-title">{m.search_results_none()}</p>
				{#if clauses.length}<p>{m.search_results_none_lead()}</p>{/if}
				{#if Recover && (clauses.length || suggested)}<Recover recovery={results.recovery} />{/if}
				{#if clauses.length}
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
				data-keys-ready={keysReady || undefined}
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
						items={rows}
						role={results.grouped.collapsed ? 'treegrid' : 'grid'}
						itemAttrs={results.grouped.collapsed ? treeAttrs : undefined}
						getKey={(r) => (G ? G.keyOf(r) : (r as SearchHit).hash)}
						label={m.search_results_label()}
						estimateSize={rowHeight}
						resetKey={results.shown ?? ''}
						bind:activeIndex={active}
						onActivate={(_i, r) => activate(r)}
						onKey={groupKey}
						hasMore={results.hasMore}
						loadingMore={results.loadingMore}
						onLoadMore={() => void results.loadMore()}
						loadMoreLabel={m.search_results_loading_more()}
					>
						{#snippet row(r)}
							{#if G && G.isGroupRow(r)}
								<div role="gridcell" class="rp-cell" data-hash={G.hitOf(r).hash}>
									<G.VersionRow row={r} testid="result-{r.key}" />
								</div>
							{:else}
								{@const hit = r as SearchHit}
								{#snippet others()}
									<Others
										others={hit.others ?? 0}
										open={expand?.isOpen(hit.hash) ?? false}
										testid="result-{hit.hash}"
										ontoggle={() => withExpand((x) => x.toggle(hit))}
									/>
								{/snippet}
								<div role="gridcell" class="rp-cell" data-hash={hit.hash}>
									{#if Pick && (selection.mode || selection.count > 0)}
										<Pick {hit} rows={hashes} />
									{/if}
									<ResultRow
										{hit}
										{density}
										{matched}
										testid="result-{hit.hash}"
										extra={hit.others && results.grouped.collapsed ? others : undefined}
									>
										{#snippet actions(h)}
											{#if Actions}<Actions hit={h} {density} testid="result-{h.hash}" />{/if}
										{/snippet}
									</ResultRow>
								</div>
							{/if}
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
