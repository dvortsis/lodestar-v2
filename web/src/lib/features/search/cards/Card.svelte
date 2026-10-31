<script lang="ts">
	// One result as a tile (§10.4 Cards density): the same hit and the same actions as the row. Opening
	// is the tile's (click, Enter); the actions are real buttons inside it. Styles: cards.css.
	import type { Snippet } from 'svelte';
	import ArrowDown from '~icons/ph/arrow-down-bold';
	import ArrowUp from '~icons/ph/arrow-up-bold';
	import History from '~icons/ph/clock-counter-clockwise-bold';
	import Warning from '~icons/ph/warning-bold';
	import { formatBytes, formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import { showBidi } from '$lib/util/bidi';
	import { ADULT_LABELS as ADULT, TYPE_LABELS as TYPE } from '$lib/util/content-labels';
	import type { SearchHit } from '../results/ResultRow.svelte';

	interface Props {
		hit: SearchHit;
		testid: string;
		onopen: (hit: SearchHit) => void;
		/** The roving tile: the one in the tab order. */
		current?: boolean;
		onfocusin?: () => void;
		actions?: Snippet<[SearchHit]>;
	}
	let { hit, testid: id, onopen, current = true, onfocusin, actions }: Props = $props();

	const locale = $derived(currentLocale());
	const title = $derived(showBidi(hit.title ?? hit.name));
	const type = $derived(
		hit.content_type === 'adult' && hit.adult_kind
			? ADULT[hit.adult_kind]?.()
			: hit.content_type
				? TYPE[hit.content_type]?.()
				: undefined
	);

	const FLAG: [string, () => string][] = [
		['private', m.search_results_flag_private],
		['v2', m.search_results_flag_v2],
		['hybrid', m.search_results_flag_hybrid],
		['imported', m.search_results_flag_imported]
	];
	const flags = $derived(FLAG.filter(([f]) => hit.flags.includes(f)));
	const scraped = $derived(hit.trackers_reporting !== null && hit.trackers_checked !== null);

	function key(event: KeyboardEvent) {
		if (event.target !== event.currentTarget || event.key !== 'Enter') return;
		event.preventDefault();
		event.stopPropagation();
		onopen(hit);
	}
	function click(event: MouseEvent) {
		if ((event.target as HTMLElement).closest('button,a')) return;
		event.stopPropagation();
		onopen(hit);
	}
</script>

<!-- The tile is the open target, as the row is for the list; its buttons stay buttons. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
	class="cd-tile"
	role="group"
	aria-label={title}
	tabindex={current ? 0 : -1}
	{onfocusin}
	data-testid={id}
	data-hash={hit.hash}
	onclick={click}
	onkeydown={key}
>
	<div class="cd-title" data-testid="{id}-title">{title}</div>
	{#if type || flags.length}
		<div class="cd-badges" data-testid="{id}-badges">
			{#if type}<span class="cd-badge" data-testid="{id}-badge-type">{type}</span>{/if}
			{#each flags as [f, label] (f)}<span class="cd-badge" data-testid="{id}-badge-{f}"
					>{label()}</span
				>{/each}
		</div>
	{/if}
	<div class="cd-facts">
		<span class="cd-fact" data-testid="{id}-size">{formatBytes(hit.size, locale)}</span>
		<span class="cd-fact" data-testid="{id}-files"
			>{(hit.files === 1 ? m.search_results_files_one : m.search_results_files_other)({
				count: formatCount(hit.files, locale)
			})}</span
		>
		<span class="cd-fact" data-testid="{id}-swarm">
			<span class="sr-only">{m.seeders()}: </span><ArrowUp aria-hidden="true" />
			{#if hit.seeders === null}
				<span role="img" aria-label={m.never_scraped()} data-testid="{id}-never-scraped">—</span>
			{:else}
				<span data-testid="{id}-seeders"
					>{formatCount({ value: hit.seeders, approx: hit.seeders_approx }, locale)}</span
				>
			{/if}
			<span class="sr-only">{m.leechers()}: </span><ArrowDown aria-hidden="true" />
			{#if hit.leechers === null}
				<span role="img" aria-label={m.never_scraped()}>—</span>
			{:else}
				<span data-testid="{id}-leechers"
					>{formatCount({ value: hit.leechers, approx: hit.seeders_approx }, locale)}</span
				>
			{/if}
		</span>
		{#if hit.swarm_suspect}
			<span class="cd-fact" data-testid="{id}-suspect">
				<Warning aria-hidden="true" />{m.swarm_suspect()}
			</span>
		{/if}
		<span class="cd-fact" data-testid="{id}-trackers">
			<span class="sr-only">{m.trackers_reporting()}: </span>
			{#if scraped}
				{m.search_results_trackers_of({
					reporting: formatCount(hit.trackers_reporting, locale),
					checked: formatCount(hit.trackers_checked, locale)
				})}
			{:else}
				<span role="img" aria-label={m.never_scraped()}>—</span>
			{/if}
		</span>
		<span class="cd-fact">
			<span class="sr-only">{m.discovered()}: </span>
			<span data-testid="{id}-discovered"><RelativeTime seconds={hit.discovered_at} /></span>
			{#if hit.flags.includes('backfill')}
				<span data-testid="{id}-backfill"><History aria-hidden="true" />{m.backfill()}</span>
			{/if}
		</span>
	</div>
	{#if actions}<div class="cd-actions rr-actions">{@render actions(hit)}</div>{/if}
</div>
