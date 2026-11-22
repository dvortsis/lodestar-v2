<script lang="ts">
	// §10.5.4 Swarm: the seeders/leechers line over time, the per-tracker table (exact lookup) and
	// the DHT estimate, labelled as one. Tracker URLs can hold a passkey, so only the host is shown,
	// in the cell and in every title or label. A tracker that sent nothing is not listed.
	import { DASH, formatCount, formatDateTime } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import type { TorrentDetail } from '../cache';
	import SwarmChart from './SwarmChart.svelte';
	import { trackerHost } from './tracker';

	let { detail, level = 3 }: { detail: TorrentDetail; level?: number } = $props();
	const id = $props.id();
	const locale = $derived(currentLocale());
	const DAY = 86_400;
	const history = $derived(detail.swarm_history);
	// Daily samples when there are enough for a line, else the weekly ones.
	const series = $derived(
		history && history.daily.length >= 2
			? { samples: history.daily, bucket: DAY }
			: history && history.weekly.length >= 2
				? { samples: history.weekly, bucket: 7 * DAY }
				: null
	);
	const trackers = $derived(detail.swarm_trackers);
	const hasDht = $derived(detail.dht_peers !== null || detail.dht_seeds !== null);
	// "Seen {relative}": the message wraps a RelativeTime, so split it around a marker.
	const MARK = '\u0000';
	const seen = $derived(m.scrape_age({ relative: MARK }).split(MARK));
	const empty = $derived(!series && trackers.length === 0 && !hasDht);
</script>

<section
	class="ls-detail-section"
	aria-labelledby="{id}-h"
	data-section="swarm"
	data-testid="detail-swarm"
>
	<div role="heading" aria-level={level} id="{id}-h">{m.detail_section_swarm()}</div>
	{#if detail.last_scrape_at !== null}
		<p class="text-small text-muted-text" data-testid="detail-scrape-age">
			{seen[0]}<RelativeTime seconds={detail.last_scrape_at} />{seen[1]}
			<span data-testid="detail-scrape-at">· {formatDateTime(detail.last_scrape_at, locale)}</span>
		</p>
	{/if}
	{#if series}
		<SwarmChart samples={series.samples} bucket={series.bucket} />
	{/if}
	{#if trackers.length > 0}
		<div class="overflow-x-auto">
			<table class="w-full text-left text-small" data-testid="detail-swarm-trackers">
				<caption class="py-1 text-left text-muted-text">{m.detail_swarm_trackers_caption()}</caption
				>
				<thead>
					<tr>
						<th scope="col" class="pe-3 font-medium">{m.detail_swarm_col_tracker()}</th>
						<th scope="col" class="px-2 text-right font-medium">{m.seeders()}</th>
						<th scope="col" class="px-2 text-right font-medium">{m.leechers()}</th>
						<th scope="col" class="px-2 text-right font-medium">{m.detail_swarm_col_completed()}</th
						>
						<th scope="col" class="ps-2 font-medium">{m.detail_swarm_col_checked()}</th>
					</tr>
				</thead>
				<tbody>
					{#each trackers as t, i (i)}
						{@const host = trackerHost(t.url)}
						<tr class="border-t border-base-300" data-testid="detail-swarm-tracker">
							<th scope="row" class="pe-3 font-normal break-all" data-testid="detail-swarm-host"
								>{host ?? m.detail_swarm_tracker_unknown()}</th
							>
							<td class="px-2 text-right tabular-nums">{formatCount(t.seeders, locale)}</td>
							<td class="px-2 text-right tabular-nums">{formatCount(t.leechers, locale)}</td>
							<td class="px-2 text-right tabular-nums">{formatCount(t.completed, locale)}</td>
							<td class="ps-2 whitespace-nowrap"><RelativeTime seconds={t.at} /></td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
	{#if hasDht}
		<dl class="ls-detail-facts" data-testid="detail-swarm-dht">
			<dt>{m.detail_swarm_dht()}</dt>
			<dd class="tabular-nums">
				{m.detail_swarm_dht_estimate({
					seeds:
						detail.dht_seeds === null
							? DASH
							: formatCount({ value: detail.dht_seeds, approx: true }, locale),
					peers:
						detail.dht_peers === null
							? DASH
							: formatCount({ value: detail.dht_peers, approx: true }, locale)
				})}
			</dd>
		</dl>
	{/if}
	{#if detail.completed !== null}
		<p class="text-small text-muted-text tabular-nums" data-testid="detail-swarm-completed">
			{m.detail_swarm_completed({ count: formatCount(detail.completed, locale) })}
		</p>
	{/if}
	{#if empty}
		<p class="text-muted-text" data-testid="detail-swarm-empty">
			{detail.last_scrape_at === null ? m.never_scraped() : m.detail_swarm_none()}
		</p>
	{/if}
</section>
