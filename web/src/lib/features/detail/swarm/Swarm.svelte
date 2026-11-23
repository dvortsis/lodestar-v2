<script lang="ts">
	// §10.5.4 Swarm: the seeders/leechers line over time, the per-tracker table (exact lookup) and
	// the DHT estimate, labelled as one. Tracker URLs can hold a passkey, so only the host is shown,
	// in the cell and in every title or label. A tracker that sent nothing is not listed.
	import { DASH, formatCount, formatDateTime } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import './swarm.css';
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
	// Any figure at all is data: a single sample, the completed count, the summary's own counts.
	const hasHistory = $derived(!!history && (history.daily.length > 0 || history.weekly.length > 0));
	const empty = $derived(
		!series &&
			!hasHistory &&
			trackers.length === 0 &&
			!hasDht &&
			detail.completed === null &&
			detail.seeders === null &&
			detail.leechers === null
	);
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
		<div class="ls-detail-scroll">
			<table class="ls-detail-table" data-testid="detail-swarm-trackers">
				<caption>{m.detail_swarm_trackers_caption()}</caption>
				<thead>
					<tr>
						<th scope="col">{m.detail_swarm_col_tracker()}</th>
						<th scope="col">{m.seeders()}</th>
						<th scope="col">{m.leechers()}</th>
						<th scope="col">{m.detail_swarm_col_completed()}</th>
						<th scope="col">{m.detail_swarm_col_checked()}</th>
					</tr>
				</thead>
				<tbody>
					{#each trackers as t, i (i)}
						{@const host = trackerHost(t.url)}
						<tr data-testid="detail-swarm-tracker">
							<th scope="row" data-testid="detail-swarm-host"
								>{host ?? m.detail_swarm_tracker_unknown()}</th
							>
							<td data-num>{formatCount(t.seeders, locale)}</td>
							<td data-num>{formatCount(t.leechers, locale)}</td>
							<td data-num>{formatCount(t.completed, locale)}</td>
							<td data-when><RelativeTime seconds={t.at} /></td>
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
	{#if detail.last_scrape_at === null}
		<p class="text-muted-text" data-testid="detail-swarm-empty">{m.never_scraped()}</p>
	{:else if empty}
		<p class="text-muted-text" data-testid="detail-swarm-empty">{m.detail_swarm_none()}</p>
	{/if}
</section>
