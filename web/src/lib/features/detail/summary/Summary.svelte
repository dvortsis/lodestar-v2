<script lang="ts">
	// §10.5.1 summary facts. Every number goes through $lib/format and has a data-testid; a null is
	// "—" (never 0), and a never-scraped swarm says so in words.
	import { DASH, formatBytes, formatCount, formatDateTime, formatScore } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import type { SearchHit, TorrentDetail } from '../cache';
	import { junkText } from './junk';

	interface Props {
		detail: TorrentDetail;
		hit: SearchHit | null;
	}
	let { detail, hit }: Props = $props();

	const locale = $derived(currentLocale());
	const flags = $derived(
		(
			[
				['private', m.detail_flag_private],
				['v2', m.detail_flag_v2],
				['hybrid', m.detail_flag_hybrid],
				['files_partial', m.detail_flag_files_partial]
			] as const
		).filter(([flag]) => detail.flags.includes(flag))
	);
	const never = $derived(detail.last_scrape_at === null);
	// "Seen {relative}": the message wraps a RelativeTime, so split it around a marker.
	const MARK = '\u0000';
	const seen = $derived(m.scrape_age({ relative: MARK }).split(MARK));
	const trackersKnown = $derived(
		detail.trackers_reporting !== null && detail.trackers_checked !== null
	);
	const reasons = $derived(hit?.junk_reasons ?? []);
</script>

<section class="ls-detail-section" aria-labelledby="detail-h-summary" data-section="summary">
	<h3 id="detail-h-summary">{m.detail_section_summary()}</h3>
	<dl class="ls-detail-facts">
		<dt>{m.detail_size()}</dt>
		<dd class="tabular-nums" data-testid="detail-size">{formatBytes(detail.size, locale)}</dd>

		<dt>{m.detail_files()}</dt>
		<dd class="tabular-nums" data-testid="detail-files">{formatCount(detail.files, locale)}</dd>

		<dt>{m.discovered()}</dt>
		<dd class="ls-detail-row">
			<span class="tabular-nums" data-testid="detail-discovered"
				>{formatDateTime(detail.discovered_at, locale)}</span
			>
			{#if detail.flags.includes('backfill')}
				<Badge>{m.backfill()}</Badge>
			{/if}
		</dd>

		<dt>{m.seeders()}</dt>
		<dd class="tabular-nums" data-testid="detail-seeders">
			{#if detail.seeders === null}
				<span aria-hidden="true">{DASH}</span><span class="sr-only">{m.never_scraped()}</span>
			{:else}
				{formatCount(detail.seeders, locale)}
			{/if}
		</dd>

		<dt>{m.leechers()}</dt>
		<dd class="tabular-nums" data-testid="detail-leechers">
			{#if detail.leechers === null}
				<span aria-hidden="true">{DASH}</span><span class="sr-only">{m.never_scraped()}</span>
			{:else}
				{formatCount(detail.leechers, locale)}
			{/if}
		</dd>

		<dt>{m.detail_swarm()}</dt>
		<dd data-testid="detail-scrape-age">
			{#if never}
				{m.never_scraped()}
			{:else}
				{seen[0]}<RelativeTime seconds={detail.last_scrape_at} />{seen[1]}
				<span class="text-muted-text" data-testid="detail-scrape-at"
					>· {formatDateTime(detail.last_scrape_at, locale)}</span
				>
			{/if}
		</dd>

		<dt>{m.trackers_reporting()}</dt>
		<dd class="tabular-nums" data-testid="detail-trackers">
			{#if trackersKnown}
				{m.detail_trackers_sentence({
					reporting: formatCount(detail.trackers_reporting, locale),
					checked: formatCount(detail.trackers_checked, locale)
				})}
			{:else}
				<span aria-hidden="true">{DASH}</span><span class="sr-only">{m.never_scraped()}</span>
			{/if}
		</dd>

		<dt>{m.detail_piece_size()}</dt>
		<dd class="tabular-nums" data-testid="detail-piece-size">
			{formatBytes(detail.piece_len, locale)}
		</dd>

		{#if flags.length}
			<dt>{m.detail_flags()}</dt>
			<dd class="ls-detail-row" data-testid="detail-flags">
				{#each flags as [flag, label] (flag)}<Badge>{label()}</Badge>{/each}
			</dd>
		{/if}

		<dt>{m.junk()}</dt>
		<dd class="ls-detail-stack">
			<span class="tabular-nums" data-testid="detail-junk-score"
				>{formatScore(hit?.junk_score ?? null, locale)}</span
			>
			{#if reasons.length}
				<ul class="ls-detail-stack" data-testid="detail-junk-reasons">
					{#each reasons as reason (reason)}<li>{junkText(reason)}</li>{/each}
				</ul>
			{/if}
		</dd>
	</dl>
</section>
