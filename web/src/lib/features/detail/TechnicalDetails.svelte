<script lang="ts">
	// "Technical details" (U14): the facts for experts, behind a disclosure so the summary leads with
	// what helps a choice. Piece size, hashes, flags, junk score with its reasons, metadata time.
	import { formatBytes, formatDateTime, formatScore } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import type { SearchHit, TorrentDetail } from './cache';

	interface Props {
		detail: TorrentDetail;
		hit: SearchHit | null;
	}
	let { detail, hit }: Props = $props();
	const id = $props.id();
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
</script>

<section class="ls-detail-section" aria-labelledby="{id}-h" data-section="technical">
	<details data-testid="detail-technical">
		<summary class="ls-focus cursor-pointer font-semibold" id="{id}-h"
			>{m.detail_technical()}</summary
		>
		<dl class="ls-detail-facts mt-3">
			<dt>{m.detail_piece_size()}</dt>
			<dd class="tabular-nums" data-testid="detail-piece-size">
				{formatBytes(detail.piece_len, locale)}
			</dd>
			<dt>{m.detail_hash()}</dt>
			<dd><Mono wrap><span data-testid="detail-hash">{detail.hash}</span></Mono></dd>
			{#if detail.hash_v2}
				<dt>{m.detail_hash_v2()}</dt>
				<dd><Mono wrap><span data-testid="detail-hash-v2">{detail.hash_v2}</span></Mono></dd>
			{/if}
			{#if flags.length}
				<dt>{m.detail_flags()}</dt>
				<dd class="ls-detail-row" data-testid="detail-flags">
					{#each flags as [flag, label] (flag)}<Badge variant="info">{label()}</Badge>{/each}
				</dd>
			{/if}
			<dt>{m.detail_metadata_at()}</dt>
			<dd class="tabular-nums" data-testid="detail-metadata-at">
				{formatDateTime(detail.metadata_at, locale)}
			</dd>
			<dt>{m.junk()}</dt>
			<dd class="ls-detail-stack">
				<span class="tabular-nums" data-testid="detail-junk-score"
					>{formatScore(hit?.junk_score ?? null, locale)}</span
				>
			</dd>
		</dl>
	</details>
</section>
