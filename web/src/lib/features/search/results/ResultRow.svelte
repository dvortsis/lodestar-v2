<script lang="ts" module>
	import type { components } from '$lib/api/schema';
	export type SearchHit = components['schemas']['SearchHit'];
	export type Density = 'comfortable' | 'compact';
</script>

<script lang="ts">
	// The shared result row (§10.4) for Search, Discover and Alerts. Comfortable is the fixed 64 px
	// three-line row (title, raw name, facts; 84 px with the "Matched in" line), Compact the 36 px
	// line (§10.1: fixed heights, so the list never measures). Styles are in row.css, which each
	// user imports: a lazily imported component's own <style> would load with /search's route.
	// Opening the row (click, Enter) is the list's (VirtualList onActivate): no links inside a row.
	import type { Snippet } from 'svelte';
	import ArrowDown from '~icons/ph/arrow-down-bold';
	import ArrowUp from '~icons/ph/arrow-up-bold';
	import History from '~icons/ph/clock-counter-clockwise-bold';
	import Warning from '~icons/ph/warning-bold';
	import CategoryBar from '$lib/charts/CategoryBar.svelte';
	import { formatBytes, formatCount, formatShare } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import { showBidi } from '$lib/util/bidi';
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
		/** A feature's own fact at the end of the facts line (Alerts: "Matched 3 h ago"). */
		extra?: Snippet;
	}
	let { hit, density, testid: id, root = true, matched = false, actions, extra }: Props = $props();

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

	// Up to 5 badges (§10.4): what the torrent is, then the flags people filter by.
	const TYPE: Record<string, () => string> = {
		movie: m.content_type_movie,
		tv: m.content_type_tv,
		music: m.content_type_music,
		audiobook: m.content_type_audiobook,
		ebook: m.content_type_ebook,
		comic: m.content_type_comic,
		software: m.content_type_software,
		game: m.content_type_game,
		images: m.content_type_images,
		video: m.content_type_video,
		other: m.content_type_other,
		adult: m.content_type_adult
	};
	const ADULT: Record<string, () => string> = {
		scene: m.adult_kind_scene,
		movie: m.adult_kind_movie,
		photos: m.adult_kind_photos,
		comics: m.adult_kind_comics,
		game: m.adult_kind_game,
		vr: m.adult_kind_vr,
		audio: m.adult_kind_audio
	};
	const FLAG: [string, () => string][] = [
		['private', m.search_results_flag_private],
		['v2', m.search_results_flag_v2],
		['hybrid', m.search_results_flag_hybrid],
		['imported', m.search_results_flag_imported]
	];
	const badges = $derived.by(() => {
		const out: { key: string; label: string }[] = [];
		const kind = hit.content_type === 'adult' && hit.adult_kind ? ADULT[hit.adult_kind] : undefined;
		const type = kind ?? (hit.content_type ? TYPE[hit.content_type] : undefined);
		if (type) out.push({ key: 'type', label: type() });
		for (const [flag, label] of FLAG)
			if (hit.flags.includes(flag)) out.push({ key: flag, label: label() });
		return out.slice(0, 5);
	});

	const comp = $derived(composition(hit.composition, formatShare, locale));
	const backfill = $derived(hit.flags.includes('backfill'));
	const scraped = $derived(hit.trackers_reporting !== null && hit.trackers_checked !== null);
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
			{#if comfortable && badges.length}
				<span class="rr-badges" data-testid="{id}-badges">
					{#each badges as b (b.key)}<span class="rr-badge" data-testid="{id}-badge-{b.key}"
							>{b.label}</span
						>{/each}
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
			<span class="rr-fact" data-testid="{id}-size">{formatBytes(hit.size, locale)}</span>
			{#if comfortable}
				<span class="rr-fact" data-testid="{id}-files"
					>{(hit.files === 1 ? m.search_results_files_one : m.search_results_files_other)({
						count: formatCount(hit.files, locale)
					})}</span
				>
			{/if}
			<span class="rr-fact" data-testid="{id}-swarm">
				<span class="sr-only">{m.seeders()}: </span><ArrowUp class="rr-icon" aria-hidden="true" />
				{#if hit.seeders === null}
					<span role="img" aria-label={m.never_scraped()} data-testid="{id}-never-scraped">—</span>
				{:else}
					<span data-testid="{id}-seeders"
						>{formatCount({ value: hit.seeders, approx: hit.seeders_approx }, locale)}</span
					>
				{/if}
				{#if comfortable}
					<span class="sr-only">{m.leechers()}: </span><ArrowDown
						class="rr-icon"
						aria-hidden="true"
					/>
					{#if hit.leechers === null}
						<span
							role="img"
							aria-label={m.never_scraped()}
							data-testid="{id}-leechers-never-scraped">—</span
						>
					{:else}
						<span data-testid="{id}-leechers"
							>{formatCount({ value: hit.leechers, approx: hit.seeders_approx }, locale)}</span
						>
					{/if}
				{/if}
				{#if hit.swarm_suspect}
					<span class="rr-fact" title={m.swarm_suspect()}>
						<Warning class="rr-icon" aria-hidden="true" />
						<span class={comfortable ? '' : 'sr-only'} data-testid="{id}-suspect"
							>{m.swarm_suspect()}</span
						>
					</span>
				{/if}
			</span>
			{#if comfortable}
				<span class="rr-fact" data-testid="{id}-trackers">
					<span class="sr-only">{m.trackers_reporting()}: </span>
					{#if scraped}
						<span aria-hidden="true"
							>{m.search_results_trackers_of({
								reporting: formatCount(hit.trackers_reporting, locale),
								checked: formatCount(hit.trackers_checked, locale)
							})}</span
						><span class="sr-only"
							>{m.search_results_trackers_reporting({
								reporting: formatCount(hit.trackers_reporting, locale),
								checked: formatCount(hit.trackers_checked, locale)
							})}</span
						>
					{:else}
						<span
							role="img"
							aria-label={m.never_scraped()}
							data-testid="{id}-trackers-never-scraped">—</span
						>
					{/if}
				</span>
			{/if}
			<span class="rr-fact">
				<span class="sr-only">{m.discovered()}: </span>
				<span data-testid="{id}-discovered"><RelativeTime seconds={hit.discovered_at} /></span>
				{#if backfill}
					<span class="rr-fact" title={m.backfill()} data-testid="{id}-backfill">
						<History class="rr-icon" aria-hidden="true" />
						<span class={comfortable ? '' : 'sr-only'}>{m.backfill()}</span>
					</span>
				{/if}
			</span>
			{#if extra}<span class="rr-fact">{@render extra()}</span>{/if}
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
