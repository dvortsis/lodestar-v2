<script lang="ts">
	// Discover (§10.7): Newest with a tab per content type, then Rising and Recently resolved.
	// The URL is the state: `?type=<tab>` and `?density=`. Every count, section and the Adult rule
	// come from `GET /discover`; the client names, lays out and links, and never filters or counts.
	import { onMount, untrack } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import Info from '~icons/ph/info-bold';
	import { api } from '$lib/api/client';
	import { formatCompact, formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import type { Route } from '$lib/shell/nav';
	import { m } from '$lib/paraglide/messages.js';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import StateScene from '$lib/shell/StateScene.svelte';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import EmptyState from '$lib/ui/feedback/EmptyState.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import VirtualList from '$lib/ui/lists/VirtualList.svelte';
	import { defaultRowHeight, tokenPx } from '$lib/ui/overlays/motion';
	import { Discover, type HitGroup, type SearchHit } from './discover.svelte';
	import ResultRow from '$lib/features/search/results/ResultRow.svelte';
	import '$lib/features/search/results/row.css';
	import { knownTab, searchHref, tabHref, tabLabel, torrentHref, typeParam, TYPES } from './tabs';
	import { unavailableText } from './unavailable';

	/** The page's query string: `type`, `density` and anything else, kept on every link. */
	let { search }: { search: URLSearchParams } = $props();

	/** §10.2: at most this many rows take part in the first-load reveal, 80 ms apart. */
	const REVEAL_ROWS = 8;
	/** Long enough for the last step and its fade; after it the rows are plain again. */
	const REVEAL_WINDOW_MS = 1500;

	const d = new Discover();
	const locale = $derived(currentLocale());
	const type = $derived(typeParam(search));
	const tab = $derived(knownTab(type));
	const density = $derived(search.get('density') === 'compact' ? 'compact' : 'comfortable');
	const rowHeight = $derived(
		density === 'compact' ? defaultRowHeight() : tokenPx('--row-height-comfortable', 64)
	);
	const skeletonRows = Array.from({ length: 8 }, (_, i) => i);

	$effect(() => {
		const t = type;
		untrack(() => d.show(t));
	});
	$effect(() => () => d.destroy());

	// The Adult tab exists only while the server says "Show adult content" is on. Until it answers,
	// and on any error, the tab stays out; what `type=adult` shows is the server's call.
	let showAdult = $state(false);
	onMount(() => {
		const ctl = new AbortController();
		api
			.get('/settings/{section}', { path: { section: 'content' }, signal: ctl.signal })
			.then((res) => (showAdult = res.fields['show_adult']?.value === true))
			.catch(() => (showAdult = false));
		return () => ctl.abort();
	});
	const tabs = $derived([null, ...TYPES.filter((t) => t !== 'adult' || showAdult)]);

	// --- the first-load reveal ------------------------------------------------------------------
	const reducedMotion = () =>
		typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
	let reveal = $state(false);
	$effect(() => {
		if (d.loads === 0 || !d.firstData || reducedMotion()) return;
		reveal = true;
		const timer = setTimeout(() => (reveal = false), REVEAL_WINDOW_MS);
		return () => clearTimeout(timer);
	});

	// --- derived copy ---------------------------------------------------------------------------
	const total = $derived.by(() => {
		const n = d.newest;
		if (!n) return '';
		return n.total_approx
			? formatCompact({ value: n.total, approx: true }, locale)
			: formatCount(n.total, locale);
	});
	const adultHidden = $derived(d.newest?.notices.includes('adult_hidden') ?? false);
	const empty = $derived(
		d.newest !== null && !adultHidden && d.newest.total === 0 && d.hits.length === 0
	);
	const showSkeleton = $derived(d.newest === null ? d.loading : d.slow);
	const lowerLabel = $derived(tab === undefined ? '' : tabLabel(tab).toLocaleLowerCase(locale));
	const announce = $derived(
		d.switched && d.newest && !adultHidden
			? m.discover_newest_total_announce({ count: total, type: tabLabel(tab ?? null) })
			: ''
	);

	// --- opening a torrent and keys -------------------------------------------------------------
	function open(hit: SearchHit) {
		const href = torrentHref(hit.hash, search);
		if (href) void goto(resolve(href as Route));
	}

	let list = $state<{ move(delta: number): void; focusActive(): void }>();
	let listBox = $state<HTMLElement>();
	function step(delta: number) {
		if (!list) return;
		if (listBox?.contains(document.activeElement)) list.move(delta);
		else list.focusActive();
	}
	// On demand, after mount: the key registry is not part of this route's first view. Only j/k
	// depend on it, and the registry bumps its own version state, which an effect would track.
	onMount(() => {
		let gone = false;
		let off: (() => void) | undefined;
		void Promise.all([import('$lib/keys'), import('./commands')]).then(
			([{ keys }, cmd]) => {
				if (!gone) off = cmd.registerDiscoverCommands(keys, step);
			},
			() => {
				// A stale build or no network: say so, since j/k would otherwise do nothing.
				if (gone) return;
				toast.push({ kind: 'error', text: m.discover_error_load() });
			}
		);
		return () => {
			gone = true;
			off?.();
		};
	});

	const linkButton = `btn btn-outline border-field-border ${FOCUS_RING} ${TARGET_MIN}`;
	const sections: { id: string; group: HitGroup | null; title: () => string; how: () => string }[] =
		$derived([
			{
				id: 'rising',
				group: d.rising,
				title: () => m.discover_rising(),
				how: () => m.discover_rising_how()
			},
			{
				id: 'resolved',
				group: d.resolved,
				title: () => m.discover_resolved(),
				how: () => m.discover_resolved_how()
			}
		]);
</script>

{#snippet failure()}
	<Banner kind="error">
		{m.discover_error_load()}
		{#snippet action()}
			<button
				type="button"
				class="btn btn-sm btn-outline border-field-border {FOCUS_RING} {TARGET_MIN}"
				onclick={() => d.retry()}>{m.discover_retry()}</button
			>
		{/snippet}
	</Banner>
{/snippet}

<div class="dsc-page" style:--row-h="{rowHeight}px">
	<h1 class="dsc-heading">{m.discover_title()}</h1>

	<nav aria-label={m.discover_tabs_label()} class="dsc-tabs-box">
		<ul class="dsc-tabs">
			{#each tabs as t (t ?? 'all')}
				<li>
					<a
						href={resolve(tabHref(t, search) as Route)}
						aria-current={tab === t ? 'page' : undefined}
						class="dsc-tab {FOCUS_RING}"
						data-testid="discover-tab-{t ?? 'all'}">{tabLabel(t)}</a
					>
				</li>
			{/each}
		</ul>
	</nav>

	{#if d.failure === 'unknown_type'}
		<EmptyState
			headingLevel={3}
			title={m.discover_unknown_type_title()}
			text={m.discover_unknown_type_text()}
		>
			{#snippet action()}
				<a href={resolve(tabHref(null, search) as Route)} class={linkButton}
					>{m.discover_unknown_type_action()}</a
				>
			{/snippet}
		</EmptyState>
	{:else if d.failure === 'search_disabled'}
		<EmptyState
			headingLevel={3}
			title={m.discover_search_off_title()}
			text={m.discover_search_off_text()}
		>
			{#snippet action()}
				<a href={resolve('/settings/search')} class={linkButton}>{m.discover_search_off_action()}</a
				>
			{/snippet}
		</EmptyState>
	{:else}
		<div class="dsc-grid">
			<section class="dsc-panel" aria-labelledby="discover-newest-h" aria-busy={d.loading}>
				<header class="dsc-panel-head">
					<h2 id="discover-newest-h" class="dsc-panel-title">{m.discover_newest()}</h2>
					{#if d.newest && !adultHidden && !showSkeleton}
						<p class="dsc-total">
							<span data-testid="discover-newest-total">{total}</span>
							{m.discover_newest_unit()}
						</p>
					{/if}
					<a
						class="dsc-see-all {FOCUS_RING}"
						href={resolve(searchHref(tab ?? null, search) as Route)}
						data-testid="discover-see-all">{m.discover_see_all()}</a
					>
				</header>
				<div class="sr-only" role="status" aria-live="polite" data-testid="discover-announce">
					{announce}
				</div>

				{#if d.failure === 'other'}
					<div class="dsc-notice-box">{@render failure()}</div>
				{/if}
				{#if d.restarted}
					<div class="dsc-notice-box">
						<Banner kind="info">{m.discover_error_restart()}</Banner>
					</div>
				{/if}
				{#if d.moreFailed}
					<div class="dsc-notice-box">
						<Banner kind="error">
							{m.discover_error_more()}
							{#snippet action()}
								<button
									type="button"
									class="btn btn-sm btn-outline border-field-border {FOCUS_RING} {TARGET_MIN}"
									onclick={() => d.retryMore()}>{m.discover_retry()}</button
								>
							{/snippet}
						</Banner>
					</div>
				{/if}

				{#if showSkeleton}
					<div class="dsc-skeletons" data-testid="discover-skeleton">
						{#each skeletonRows as i (i)}
							<Skeleton class="dsc-skeleton-row" />
						{/each}
					</div>
				{:else if adultHidden}
					<div class="dsc-notice-box">
						<Banner kind="info" title={tabLabel('adult')}>
							{m.discover_adult_hidden()}
							{#snippet action()}
								<a
									href={resolve('/settings/content')}
									class="btn btn-sm btn-outline border-field-border {FOCUS_RING} {TARGET_MIN}"
									>{m.discover_adult_hidden_action()}</a
								>
							{/snippet}
						</Banner>
					</div>
				{:else if empty}
					{#if tab === null}
						<EmptyState
							headingLevel={3}
							title={m.discover_empty_title()}
							text={m.discover_empty_text()}
						>
							{#snippet illustration()}<StateScene id="empty_state_explore" />{/snippet}
							{#snippet action()}
								<a href={resolve('/insights')} class={linkButton}>{m.discover_empty_action()}</a>
							{/snippet}
						</EmptyState>
					{:else}
						<EmptyState
							title={m.discover_empty_tab_title({ type: lowerLabel })}
							text={m.discover_empty_tab_text()}
						>
							{#snippet action()}
								<a href={resolve(tabHref(null, search) as Route)} class={linkButton}
									>{m.discover_empty_tab_action()}</a
								>
							{/snippet}
						</EmptyState>
					{/if}
				{:else if d.hits.length > 0}
					<div
						class="dsc-list"
						class:dsc-complete={!d.hasMore}
						style:--loaded-height="{d.hits.length * rowHeight}px"
						bind:this={listBox}
					>
						<VirtualList
							bind:this={list}
							items={d.hits}
							getKey={(hit) => hit.hash}
							label={m.discover_newest_label()}
							estimateSize={rowHeight}
							totalCount={d.newest?.total_approx ? undefined : d.newest?.total}
							resetKey="{type}:{d.loads}"
							hasMore={d.hasMore}
							loadingMore={d.loadingMore}
							onLoadMore={() => d.more()}
							loadMoreLabel={m.discover_loading_more()}
							onActivate={(_, hit) => open(hit)}
						>
							{#snippet row(hit, index)}
								<div
									class="dsc-cell"
									class:dsc-reveal={reveal && d.firstData && index < REVEAL_ROWS}
									style:--i={index}
									data-testid="discover-row-{hit.hash}"
								>
									<ResultRow {hit} {density} testid="discover-row-{hit.hash}" root={false} />
								</div>
							{/snippet}
						</VirtualList>
					</div>
				{/if}
			</section>

			<div class="dsc-side">
				{#each sections as s (s.id)}
					<section
						class="dsc-panel"
						aria-labelledby="discover-{s.id}-h"
						data-testid="discover-{s.id}"
					>
						<header class="dsc-panel-head">
							<h2 id="discover-{s.id}-h" class="dsc-panel-title">{s.title()}</h2>
						</header>
						{#if s.group === null}
							{#if d.loading}<div class="dsc-skeletons">
									<Skeleton class="dsc-skeleton-row" />
								</div>{/if}
						{:else if s.group.status === 'unavailable'}
							{@const why = s.group.reason ? unavailableText(s.group.reason) : null}
							<div class="dsc-unavailable" data-testid="discover-{s.id}-unavailable">
								<Info class="dsc-icon" aria-hidden="true" />
								<div>
									<p class="dsc-caption">{m.discover_unavailable_label()}</p>
									{#if why}<p>{why.text}</p>
										{#if why.action}<a
												class="dsc-inline-link {FOCUS_RING}"
												href={resolve(why.action.href as Route)}>{why.action.label}</a
											>{/if}
									{/if}
								</div>
							</div>
						{:else}
							<p class="dsc-how">{s.how()}</p>
							<ul class="dsc-plain" aria-label={s.title()}>
								{#each s.group.hits as hit (hit.hash)}
									{@const href = torrentHref(hit.hash, search)}
									<li style:height="{rowHeight}px">
										{#if href}
											<a
												href={resolve(href as Route)}
												class="dsc-row-link {FOCUS_RING}"
												data-testid="discover-{s.id}-{hit.hash}"
												><ResultRow {hit} {density} testid="discover-{s.id}-row-{hit.hash}" /></a
											>
										{:else}
											<ResultRow {hit} {density} testid="discover-{s.id}-row-{hit.hash}" />
										{/if}
									</li>
								{/each}
							</ul>
							{#if s.group.truncated}<p class="dsc-how">{m.discover_truncated()}</p>{/if}
						{/if}
					</section>
				{/each}
			</div>
		</div>
	{/if}
</div>

<style>
	@reference '../../../app.css';

	.dsc-page {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 4);
		box-sizing: border-box;
		width: 100%;
		padding: calc(var(--spacing) * 4);
	}
	@variant large {
		.dsc-page {
			padding: calc(var(--spacing) * 6);
			gap: calc(var(--spacing) * 6);
		}
	}
	.dsc-heading {
		font-size: var(--text-display);
		line-height: var(--text-display--line-height);
		letter-spacing: var(--text-display--letter-spacing);
		font-weight: var(--font-weight-semibold);
	}
	.dsc-tabs-box {
		max-width: 100%;
		overflow-x: auto;
		border-bottom: var(--border) solid var(--color-base-300);
	}
	.dsc-tabs {
		display: flex;
		gap: calc(var(--spacing) * 1);
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.dsc-tab {
		display: inline-flex;
		align-items: center;
		box-sizing: border-box;
		min-height: var(--control-height);
		padding-inline: calc(var(--spacing) * 3);
		border-bottom: calc(var(--spacing) * 0.5) solid transparent;
		white-space: nowrap;
		color: var(--color-muted-text);
		font-size: var(--text-body);
	}
	.dsc-tab:focus-visible,
	.dsc-row-link:focus-visible {
		outline-offset: calc(var(--focus-ring-width) * -1);
	}
	.dsc-tab:hover {
		color: var(--color-base-content);
	}
	.dsc-tab[aria-current='page'] {
		color: var(--color-base-content);
		border-bottom-color: var(--color-primary);
		font-weight: var(--font-weight-semibold);
	}
	@media (pointer: coarse) {
		.dsc-tab {
			min-height: var(--target-min);
			min-width: var(--target-min);
			justify-content: center;
		}
	}
	.dsc-grid {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: calc(var(--spacing) * 6);
		align-items: start;
	}
	@variant x-large {
		.dsc-grid {
			grid-template-columns: minmax(0, 2fr) minmax(0, 1fr);
		}
	}
	.dsc-side {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 6);
		min-width: 0;
	}
	.dsc-panel {
		min-width: 0;
		border: var(--border) solid var(--color-base-300);
		border-radius: var(--radius-box);
		background: var(--color-base-100);
		overflow: hidden;
	}
	.dsc-panel-head {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: calc(var(--spacing) * 1) calc(var(--spacing) * 3);
		padding: calc(var(--spacing) * 3) calc(var(--spacing) * 4);
	}
	.dsc-panel-title {
		font-size: var(--text-title);
		line-height: var(--text-title--line-height);
		font-weight: var(--font-weight-semibold);
	}
	.dsc-total {
		color: var(--color-muted-text);
		font-size: var(--text-body);
		font-variant-numeric: tabular-nums;
	}
	.dsc-see-all,
	.dsc-inline-link {
		color: var(--color-primary);
		text-decoration: underline;
		font-size: var(--text-body);
	}
	.dsc-see-all {
		margin-inline-start: auto;
	}
	@media (pointer: coarse) {
		.dsc-see-all,
		.dsc-inline-link {
			display: inline-flex;
			align-items: center;
			min-height: var(--target-min);
		}
	}
	.dsc-notice-box {
		padding: 0 calc(var(--spacing) * 4) calc(var(--spacing) * 3);
	}
	.dsc-list {
		height: calc(100dvh - calc(var(--spacing) * 14 * 4));
		min-height: calc(var(--row-height-comfortable) * 6);
	}
	.dsc-list.dsc-complete {
		height: min(
			var(--loaded-height),
			max(calc(100dvh - var(--spacing) * 56), calc(var(--row-height-comfortable) * 6))
		);
		min-height: 0;
	}
	.dsc-cell {
		position: relative;
		height: 100%;
		/* The rule between rows takes no height: the shared row fills the cell's 64 px exactly. */
		box-shadow: inset 0 var(--border) 0 var(--color-base-300);
	}
	.dsc-skeletons {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 1);
		padding: 0 calc(var(--spacing) * 4) calc(var(--spacing) * 4);
	}
	.dsc-skeletons :global(.dsc-skeleton-row) {
		height: var(--row-h);
	}
	.dsc-how {
		padding: 0 calc(var(--spacing) * 4) calc(var(--spacing) * 3);
		color: var(--color-muted-text);
		font-size: var(--text-small);
	}
	.dsc-plain {
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.dsc-plain li {
		box-shadow: inset 0 var(--border) 0 var(--color-base-300); /* no height: see .dsc-cell */
	}
	.dsc-row-link {
		display: block;
		height: 100%;
		color: inherit;
		text-decoration: none;
	}
	.dsc-row-link:hover {
		background: var(--color-selected);
	}
	.dsc-unavailable {
		display: flex;
		gap: calc(var(--spacing) * 3);
		padding: 0 calc(var(--spacing) * 4) calc(var(--spacing) * 4);
		color: var(--color-muted-text);
		font-size: var(--text-body);
	}
	.dsc-unavailable :global(.dsc-icon) {
		flex: none;
		width: var(--icon-size-nav);
		height: var(--icon-size-nav);
	}
	.dsc-caption {
		font-size: var(--text-caption);
		font-weight: var(--font-weight-semibold);
		text-transform: none;
	}

	/*
	 * The first-load reveal (§10.2). The rows stay readable and focusable from the start; a cover in
	 * the panel's own colour fades off each of the first rows in turn (the row's own opacity never
	 * changes, so contrast checks still see the real text colour), with a short rise.
	 */
	@media (prefers-reduced-motion: no-preference) {
		.dsc-reveal {
			animation: discover-rise calc(var(--duration-fast) * 2) var(--ease-out) backwards;
			animation-delay: calc(var(--duration-stagger) * var(--i));
		}
		.dsc-reveal::after {
			content: '';
			position: absolute;
			inset: 0;
			pointer-events: none;
			background: var(--color-base-100);
			animation: discover-uncover calc(var(--duration-fast) * 2) var(--ease-out) both;
			animation-delay: calc(var(--duration-stagger) * var(--i));
		}
	}
	@keyframes discover-rise {
		from {
			transform: translateY(calc(var(--spacing) * 1));
		}
	}
	@keyframes discover-uncover {
		from {
			opacity: 1;
		}
		to {
			opacity: 0;
		}
	}
</style>
