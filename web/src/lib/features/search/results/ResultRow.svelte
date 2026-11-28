<script lang="ts" module>
	import type { components } from '$lib/api/schema';
	export type SearchHit = components['schemas']['SearchHit'];
	export type Density = 'comfortable' | 'compact';
	// What the Cards tile and the live banner use of the row's own helpers: they import them from
	// here, not from the modules, so the bundler keeps them in this chunk (a second importer would
	// split each one into its own tiny chunk, which Alerts and Discover would then load too).
	import { formatBytes, formatCount } from '$lib/format';
	import { showBidi } from '$lib/util/bidi';
	import { ADULT_LABELS as ADULT, TYPE_LABELS as TYPE } from '$lib/util/content-labels';
	// The Svelte parser reads an `export { imported }` in a module script as an assignment to it.
	// eslint-disable-next-line no-import-assign
	export { ADULT as ADULT_LABELS, TYPE as TYPE_LABELS, formatBytes, formatCount, showBidi };
</script>

<script lang="ts">
	// The shared result row (§10.4) for Search, Discover and Alerts. Comfortable is the fixed 64 px
	// three-line row (title, raw name, facts; 84 px with the "Matched in" line), Compact the 36 px
	// line (§10.1: fixed heights, so the list never measures). Styles are in row.css, which each
	// user imports: a lazily imported component's own <style> would load with /search's route.
	// Opening the row (click, Enter) is the list's (VirtualList onActivate): no links inside a row.
	import type { Snippet } from 'svelte';
	import ArrowUp from '~icons/ph/arrow-up-bold';
	import History from '~icons/ph/clock-counter-clockwise-bold';
	import Warning from '~icons/ph/warning-bold';
	import CategoryBar from '$lib/charts/CategoryBar.svelte';
	import { formatShare } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import UnverifiedBadge from '../unverified/UnverifiedBadge.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import { badgesOf, collapseBadges, ROW_FILTER } from './badges';
	import { composition } from './composition';

	interface Props {
		hit: SearchHit;
		density: Density;
		/** Prefix of every data-testid in the row, and the row's own (unless `root` is false). */
		testid: string;
		/** False when the container already carries `testid` (Discover's bordered cell). */
		root?: boolean;
		/** The list's query has a file clause: show "Matched in N files" (the 84 px row). */
		matched?: boolean;
		/** Search's row actions (copy, more), loaded lazily by the list; Discover and Alerts have none. */
		actions?: Snippet<[SearchHit]>;
		/** Discover's Newest: say a discovery from the first crawl was backfill (recency is its point). */
		backfill?: boolean;
		/** A feature's own fact, first on the facts line so it never clips (Alerts: "Matched 3 h ago"). */
		extra?: Snippet;
	}
	let {
		hit,
		density,
		testid: id,
		root = true,
		matched = false,
		backfill = false,
		actions,
		extra
	}: Props = $props();

	const locale = $derived(currentLocale());
	const comfortable = $derived(density === 'comfortable');
	// Torrent names are untrusted text: bidirectional controls show as markers (§20).
	const title = $derived(showBidi(hit.title ?? hit.name));
	const rawName = $derived(
		hit.title !== null && hit.title !== hit.name ? showBidi(hit.name) : null
	);
	const VIA: Record<string, () => string> = {
		files: m.search_results_resolved_via_files,
		folder: m.search_results_resolved_via_folder,
		structure: m.search_results_resolved_via_structure
	};
	const via = $derived(
		hit.resolved_via === null
			? null
			: (VIA[hit.resolved_via]?.() ??
					m.search_results_resolved_via_other({ method: hit.resolved_via }))
	);

	// Up to 5 badges (§10.4): safety and verification never give way; the rest collapse into "+N".
	const all = $derived(
		badgesOf(hit, {
			type: (key, adult) =>
				(key === 'adult' && adult ? ADULT[adult] : undefined)?.() ?? TYPE[key]?.(),
			private: m.search_results_flag_private(),
			imported: m.search_results_flag_imported(),
			v2: m.search_results_flag_v2(),
			hybrid: m.search_results_flag_hybrid()
		})
	);
	const collapsed = $derived(collapseBadges(all));
	// Compact has no badge line: only what must never go missing (unverified, suspect) shows.
	const shown = $derived(comfortable ? collapsed.shown : all.filter((b) => b.keep));
	// Search passes row actions and so can filter; Discover and Alerts rows show plain tags.
	const filterable = $derived(actions !== undefined);
	// The filter itself is the facet host's (it owns the pending edit and the AST path): the row only
	// says which facet and value, so a Discover or Alerts page never loads the search state for it.
	function filterBy(facet: string, value: string, event: MouseEvent) {
		event.stopPropagation(); // not the row's activation (it would open the drawer)
		window.dispatchEvent(new CustomEvent(ROW_FILTER, { detail: { facet, value } }));
	}

	const comp = $derived(composition(hit.composition, formatShare, locale));
	const matchedCount = $derived(hit.matched_files?.length ?? 0);
</script>

<div
	class="rr-row"
	class:rr-compact={!comfortable}
	data-density={density}
	data-testid={root ? id : undefined}
>
	<div class="rr-main">
		<div class="rr-title" data-testid="{id}-title">{title}</div>
		{#if comfortable && rawName !== null}
			<div class="rr-raw">
				<Mono class="rr-raw-name"><span data-testid="{id}-raw">{rawName}</span></Mono>
				{#if via}<span class="rr-via" data-testid="{id}-via">{via}</span>{/if}
			</div>
		{/if}
		<div class="rr-facts">
			<!-- A feature's own fact first. One that doesn't fit drops whole (row.css), never mid-word. -->
			{#if extra}<span class="rr-fact rr-extra">{@render extra()}</span>{/if}
			{#if shown.length}
				<span class="rr-badges" data-testid="{id}-badges">
					{#each shown as b (b.key)}
						{#if b.kind === 'unverified'}
							<UnverifiedBadge imported={hit.imported === true} />
						{:else if b.kind === 'suspect'}
							<span class="rr-badge" title={m.swarm_suspect()} data-testid="{id}-suspect-badge">
								<Warning class="rr-icon" aria-hidden="true" />
								<span class={comfortable ? '' : 'sr-only'} data-testid="{id}-suspect"
									>{m.swarm_suspect()}</span
								>
							</span>
						{:else if filterable && b.filter}
							{@const f = b.filter}
							<button
								type="button"
								class="rr-badge rr-badge-btn rr-optional"
								tabindex="-1"
								aria-label={m.search_results_badge_filter({ label: b.label })}
								data-testid="{id}-badge-{b.key}"
								onclick={(e) => filterBy(f.facet, f.value, e)}>{b.label}</button
							>
						{:else}
							<span class="rr-badge rr-optional" data-testid="{id}-badge-{b.key}">{b.label}</span>
						{/if}
					{/each}
					{#if comfortable && collapsed.hidden !== null}
						<!-- Opens the row (the click bubbles to the list's activation): the drawer holds the rest. -->
						<button
							type="button"
							class="rr-badge rr-badge-btn rr-optional"
							tabindex="-1"
							aria-label={m.search_results_badges_more({
								count: formatCount(collapsed.hidden, locale)
							})}
							data-testid="{id}-badges-more">+{formatCount(collapsed.hidden, locale)}</button
						>
					{/if}
				</span>
			{/if}
			{#if comfortable}
				<span class="rr-fact rr-comp" data-testid="{id}-composition">
					{#if comp}
						<span class="rr-bar"
							><CategoryBar
								segments={comp.segments}
								summary={comp.summary}
								testid="{id}-compbar"
							/></span
						>
						<span data-testid="{id}-dominant">{comp.dominant}</span>
					{:else}
						<span>{m.search_results_not_classified()}</span>
					{/if}
				</span>
			{/if}
			<span class="rr-fact rr-size" data-testid="{id}-size">{formatBytes(hit.size, locale)}</span>
			{#if comfortable}
				<span class="rr-fact rr-files" data-testid="{id}-files"
					>{(hit.files === 1 ? m.search_results_files_one : m.search_results_files_other)({
						count: formatCount(hit.files, locale)
					})}</span
				>
			{/if}
			<span class="rr-fact rr-swarm" data-testid="{id}-swarm">
				{#if !comfortable || hit.seeders === null}
					<span class="sr-only">{m.seeders()}: </span>
				{/if}
				{#if !comfortable}<ArrowUp class="rr-icon" aria-hidden="true" />{/if}
				{#if hit.seeders === null}
					<span role="img" aria-label={m.never_scraped()} data-testid="{id}-never-scraped">—</span>
				{:else}
					<span data-testid="{id}-seeders"
						>{comfortable
							? (hit.seeders === 1 ? m.search_results_seeders_one : m.search_results_seeders_other)(
									{
										count: formatCount({ value: hit.seeders, approx: hit.seeders_approx }, locale)
									}
								)
							: formatCount({ value: hit.seeders, approx: hit.seeders_approx }, locale)}</span
					>
				{/if}
			</span>
			<span class="rr-fact rr-discovered">
				<span class="sr-only">{m.discovered()}: </span>
				<span data-testid="{id}-discovered"><RelativeTime seconds={hit.discovered_at} /></span>
				{#if backfill && hit.flags.includes('backfill')}
					<span class="rr-fact" title={m.backfill()} data-testid="{id}-backfill">
						<History class="rr-icon" aria-hidden="true" />
						<span class={comfortable ? '' : 'sr-only'}>{m.backfill()}</span>
					</span>
				{/if}
			</span>
		</div>
		{#if comfortable && matched}
			<div class="rr-matched" data-testid="{id}-matched">
				{#if matchedCount > 0}
					{(matchedCount === 1
						? m.search_results_matched_files_one
						: m.search_results_matched_files_other)({ count: formatCount(matchedCount, locale) })}
				{/if}
			</div>
		{/if}
	</div>
	{#if actions}<div class="rr-actions">{@render actions(hit)}</div>{/if}
</div>
