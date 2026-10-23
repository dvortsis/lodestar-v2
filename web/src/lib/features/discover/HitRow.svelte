<script lang="ts">
	// TODO(S-search-2): replace with the shared result row.
	// §10.4's Comfortable facts, without badges or actions: Discover only opens a torrent.
	// Comfortable is the 64 px three-line row (title, raw name, facts); Compact the 36 px line.
	import ArrowDown from '~icons/ph/arrow-down-bold';
	import ArrowUp from '~icons/ph/arrow-up-bold';
	import History from '~icons/ph/clock-counter-clockwise-bold';
	import { m } from '$lib/paraglide/messages.js';
	import { CategoryBar } from '$lib/charts';
	import { formatBytes, formatCount, formatShare } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import type { SearchHit } from './discover.svelte';
	import { composition } from './composition';

	interface Props {
		hit: SearchHit;
		density: 'comfortable' | 'compact';
	}
	let { hit, density }: Props = $props();

	const locale = $derived(currentLocale());
	const title = $derived(hit.title ?? hit.name);
	const rawName = $derived(hit.title !== null && hit.title !== hit.name ? hit.name : null);
	const via = $derived(
		hit.resolved_via === null
			? null
			: hit.resolved_via === 'files'
				? m.discover_resolved_via_files()
				: hit.resolved_via === 'folder'
					? m.discover_resolved_via_folder()
					: hit.resolved_via === 'structure'
						? m.discover_resolved_via_structure()
						: m.discover_resolved_via_other({ method: hit.resolved_via })
	);
	const comp = $derived(composition(hit.composition, formatShare, locale));
	const backfill = $derived(hit.flags.includes('backfill'));
	const id = $derived(`discover-row-${hit.hash}`);
	const scraped = $derived(hit.trackers_reporting !== null && hit.trackers_checked !== null);
</script>

<div class="dsc-row" class:dsc-compact={density === 'compact'} data-density={density}>
	<div class="dsc-title" data-testid="{id}-title">{title}</div>
	{#if density === 'comfortable' && rawName !== null}
		<div class="dsc-raw">
			<Mono class="dsc-raw-name">{rawName}</Mono>
			{#if via}<span class="dsc-via">{via}</span>{/if}
		</div>
	{/if}
	<div class="dsc-facts">
		<span class="dsc-fact" data-testid="{id}-size">{formatBytes(hit.size, locale)}</span>
		<span class="dsc-fact" data-testid="{id}-swarm">
			<span class="sr-only">{m.seeders()}: </span><ArrowUp class="dsc-icon" aria-hidden="true" />
			{#if hit.seeders === null}
				<span role="img" aria-label={m.never_scraped()} data-testid="{id}-never-scraped">—</span>
			{:else}
				<span data-testid="{id}-seeders">{formatCount(hit.seeders, locale)}</span>
			{/if}
			<span class="sr-only">{m.leechers()}: </span><ArrowDown class="dsc-icon" aria-hidden="true" />
			{#if hit.leechers === null}
				<span role="img" aria-label={m.never_scraped()} data-testid="{id}-leechers-never-scraped"
					>—</span
				>
			{:else}
				<span data-testid="{id}-leechers">{formatCount(hit.leechers, locale)}</span>
			{/if}
		</span>
		{#if density === 'comfortable'}
			<span class="dsc-fact" data-testid="{id}-files"
				>{m.discover_files({ count: formatCount(hit.files, locale) })}</span
			>
			<span class="dsc-fact" data-testid="{id}-trackers">
				<span class="sr-only">{m.trackers_reporting()}: </span>
				{#if scraped}
					<span aria-hidden="true"
						>{m.discover_trackers_of({
							reporting: formatCount(hit.trackers_reporting, locale),
							checked: formatCount(hit.trackers_checked, locale)
						})}</span
					><span class="sr-only"
						>{m.discover_trackers_reporting({
							reporting: formatCount(hit.trackers_reporting, locale),
							checked: formatCount(hit.trackers_checked, locale)
						})}</span
					>
				{:else}
					<span role="img" aria-label={m.never_scraped()}>—</span>
				{/if}
			</span>
			<span class="dsc-fact dsc-comp" data-testid="{id}-composition">
				{#if comp}
					<span class="dsc-bar"
						><CategoryBar
							segments={comp.segments}
							summary={comp.summary}
							testid="{id}-compbar"
						/></span
					>
					<span data-testid="{id}-dominant">{comp.dominant}</span>
				{:else}
					<span class="dsc-muted">{m.discover_not_classified()}</span>
				{/if}
			</span>
		{/if}
		<span class="dsc-fact">
			<span class="sr-only">{m.discovered()}: </span>
			<span data-testid="{id}-discovered"><RelativeTime seconds={hit.discovered_at} /></span>
			{#if backfill}
				<History class="dsc-icon" aria-hidden="true" />
				{#if density === 'comfortable'}<span class="dsc-muted">{m.backfill()}</span>
				{:else}<span class="sr-only">{m.backfill()}</span>{/if}
			{/if}
		</span>
	</div>
</div>

<style>
	.dsc-row {
		display: flex;
		flex-direction: column;
		justify-content: center;
		box-sizing: border-box;
		height: 100%;
		min-width: 0;
		padding-inline: calc(var(--spacing) * 3);
		padding-block: var(--spacing);
		overflow: hidden;
		color: var(--color-base-content);
	}
	.dsc-row.dsc-compact {
		flex-direction: row;
		align-items: center;
		gap: calc(var(--spacing) * 3);
	}
	.dsc-title {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--text-body);
		line-height: var(--text-body--line-height);
		font-weight: var(--font-weight-medium);
	}
	.dsc-compact .dsc-title {
		flex: 1 1 0;
	}
	.dsc-raw {
		display: flex;
		gap: calc(var(--spacing) * 2);
		min-width: 0;
		overflow: hidden;
		white-space: nowrap;
		color: var(--color-muted-text);
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
	}
	.dsc-raw :global(.dsc-raw-name) {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.dsc-via {
		flex: none;
	}
	.dsc-facts {
		display: flex;
		align-items: center;
		gap: calc(var(--spacing) * 4);
		min-width: 0;
		overflow: hidden;
		white-space: nowrap;
		color: var(--color-muted-text);
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
		font-variant-numeric: tabular-nums;
	}
	.dsc-compact .dsc-facts {
		flex: none;
		gap: calc(var(--spacing) * 3);
	}
	.dsc-fact {
		display: inline-flex;
		align-items: center;
		gap: var(--spacing);
		flex: none;
	}
	.dsc-fact :global(.dsc-icon) {
		width: var(--icon-size-row);
		height: var(--icon-size-row);
		flex: none;
	}
	.dsc-bar {
		flex: none;
		width: var(--compbar-min-width);
	}
	.dsc-muted {
		color: var(--color-muted-text);
	}
</style>
