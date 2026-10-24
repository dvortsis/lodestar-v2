<script lang="ts">
	// §10.5.2 composition: the fixed-order bar with its text, and the sorted bar list underneath.
	// Shares are the server's whole percents; the client sums nothing.
	import CategoryBar from '$lib/charts/CategoryBar.svelte';
	import HBarList from '$lib/charts/HBarList.svelte';
	import { GROUP_COLOR, GROUP_ORDER } from '$lib/charts/shared';
	import { formatBytes, formatCount, formatShare } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { SearchHit } from '../cache';
	import { groupLabel, groupOf } from './groups';

	interface Props {
		/** undefined: the hit is not known (yet); []: padding only. */
		composition: SearchHit['composition'] | undefined;
	}
	let { composition }: Props = $props();

	const locale = $derived(currentLocale());
	const rows = $derived((composition ?? []).map((c) => ({ ...c, group: groupOf(c.group) })));
	// Biggest share first for the text; the bar draws in the fixed §10.18 order by itself.
	const byShare = $derived(
		[...rows].sort(
			(a, b) =>
				b.pct_bytes - a.pct_bytes || GROUP_ORDER.indexOf(a.group) - GROUP_ORDER.indexOf(b.group)
		)
	);
	const share = (r: (typeof rows)[number]) =>
		m.dominant_share({
			share: formatShare(r.pct_bytes, locale),
			group: groupLabel(r.group).toLocaleLowerCase(locale)
		});
	const summary = $derived(byShare.map(share).join(', '));
	const dominant = $derived(byShare[0] ? share(byShare[0]) : undefined);
</script>

<div class="ls-detail-stack" data-testid="detail-composition">
	<h4 class="font-medium">{m.composition()}</h4>
	{#if composition === undefined}
		<p class="text-muted-text" data-testid="detail-composition-unknown">
			{m.detail_composition_unknown()}
		</p>
	{:else if rows.length === 0}
		<p class="text-muted-text" data-testid="detail-composition-empty">
			{m.detail_composition_empty()}
		</p>
	{:else}
		<CategoryBar
			segments={rows.map((r) => ({ group: r.group, value: r.bytes, label: groupLabel(r.group) }))}
			{summary}
			{dominant}
			testid="detail-compbar"
		/>
		<HBarList
			label={m.composition()}
			items={rows.map((r) => ({
				key: r.group,
				label: m.detail_group_files({
					group: groupLabel(r.group),
					files: formatCount(r.files, locale)
				}),
				value: r.bytes,
				share: r.pct_bytes / 100,
				color: GROUP_COLOR[r.group]
			}))}
			format={formatBytes}
			testid="detail-groups"
		/>
	{/if}
</div>
