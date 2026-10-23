<script lang="ts">
	// TODO(S-search-2): replace with the shared result row
	// §10.4's Comfortable facts for one alert: the resolved title (the raw name in mono beneath when
	// it differs), the composition bar with its dominant share, size, files, seeders and leechers,
	// trackers reporting, discovered, and when the alert matched. Null states are as §10.14 says:
	// "—" with "Never scraped", "Not classified yet". Opening the row (click, Enter) is the list's.
	import CategoryBar from '$lib/charts/CategoryBar.svelte';
	import type { Group } from '$lib/charts/shared';
	import { formatBytes, formatCount, formatShare } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import type { AlertRow } from './api';

	let { row }: { row: AlertRow } = $props();

	const t = $derived(row.torrent);
	const locale = $derived(currentLocale());
	const id = $derived(row.id);

	// TODO(S-detail): the group labels and the dominant share are shared messages once S-detail adds
	// them to lib/i18n/messages; until then they are this feature's own.
	const GROUP_LABEL: Record<Group, () => string> = {
		video: () => m.alerts_group_video(),
		audio: () => m.alerts_group_audio(),
		images: () => m.alerts_group_images(),
		documents: () => m.alerts_group_documents(),
		archives: () => m.alerts_group_archives(),
		software: () => m.alerts_group_software(),
		other: () => m.alerts_group_other()
	};
	const label = (g: string) => (GROUP_LABEL[g as Group] ?? GROUP_LABEL.other)();

	const segments = $derived(
		t.composition.map((c) => ({ group: c.group as Group, value: c.bytes, label: label(c.group) }))
	);
	// The server's biggest share; the order is the bar's, so this only picks one of its numbers.
	const dominant = $derived(
		t.composition.reduce<(typeof t.composition)[number] | undefined>(
			(best, c) => (best === undefined || c.pct_bytes > best.pct_bytes ? c : best),
			undefined
		)
	);
	const dominantText = $derived(
		dominant
			? m.alerts_dominant_share({
					share: formatShare(dominant.pct_bytes, locale),
					group: label(dominant.group)
				})
			: undefined
	);
	const summary = $derived(
		m.alerts_composition_summary({
			parts: t.composition
				.map((c) =>
					m.alerts_dominant_share({
						share: formatShare(c.pct_bytes, locale),
						group: label(c.group)
					})
				)
				.join(', ')
		})
	);
</script>

<div class="al-hit" data-testid="alert-row-{id}">
	<p class="al-title" data-testid="alert-title-{id}">{t.title ?? t.name}</p>
	{#if t.title !== null && t.title !== t.name}
		<Mono class="al-raw" wrap><span data-testid="alert-raw-{id}">{t.name}</span></Mono>
	{/if}
	<div class="al-facts">
		<span class="al-comp">
			{#if t.composition.length > 0}
				<CategoryBar {segments} {summary} dominant={dominantText} testid="alert-comp-{id}" />
			{:else}
				<span class="al-muted" data-testid="alert-comp-{id}">{m.alerts_composition_none()}</span>
			{/if}
		</span>
		<span data-testid="alert-size-{id}">{formatBytes(t.size, locale)}</span>
		<span data-testid="alert-files-{id}">
			{t.files === 1
				? m.alerts_files_one()
				: m.alerts_files_many({ count: formatCount(t.files, locale) })}
		</span>
		<span data-testid="alert-swarm-{id}">
			{#if t.seeders === null && t.leechers === null}
				<span aria-hidden="true">—</span><span class="sr-only">{m.never_scraped()}</span>
			{:else}
				{t.seeders_approx ? m.estimate() : ''}{m.alerts_swarm({
					seeders: formatCount(t.seeders, locale),
					leechers: formatCount(t.leechers, locale)
				})}
				{#if t.swarm_suspect}<span data-testid="alert-suspect-{id}">· {m.swarm_suspect()}</span
					>{/if}
			{/if}
		</span>
		<span data-testid="alert-trackers-{id}">
			{#if t.trackers_reporting === null || t.trackers_checked === null}
				<span aria-hidden="true">—</span><span class="sr-only">{m.never_scraped()}</span>
			{:else}
				<span aria-hidden="true"
					>{m.alerts_trackers({
						reporting: formatCount(t.trackers_reporting, locale),
						checked: formatCount(t.trackers_checked, locale)
					})}</span
				><span class="sr-only"
					>{m.alerts_trackers_aria({
						reporting: formatCount(t.trackers_reporting, locale),
						checked: formatCount(t.trackers_checked, locale)
					})}</span
				>
			{/if}
		</span>
		<span>{m.discovered()} <RelativeTime seconds={t.discovered_at} /></span>
		<span data-testid="alert-matched-{id}"
			>{m.alerts_matched()} <RelativeTime seconds={row.created_at} /></span
		>
	</div>
</div>

<style>
	.al-hit {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 1);
		padding: calc(var(--spacing) * 3) calc(var(--spacing) * 4);
		border-bottom: var(--border) solid var(--color-base-300);
	}
	.al-title {
		margin: 0;
		overflow-wrap: anywhere;
		font-weight: var(--font-weight-medium);
	}
	.al-hit :global(.al-raw) {
		color: var(--color-muted-text);
	}
	.al-facts {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: calc(var(--spacing) * 1) calc(var(--spacing) * 4);
		color: var(--color-muted-text);
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
		font-variant-numeric: tabular-nums;
	}
	.al-comp {
		inline-size: var(--compbar-max-width);
		min-inline-size: var(--compbar-min-width);
	}
	.al-muted {
		color: var(--color-muted-text);
	}
</style>
