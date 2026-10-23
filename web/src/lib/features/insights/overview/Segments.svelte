<script lang="ts">
	// Level 4, Segments (§10.9.2): what is arriving, from where. Group and type bars count newly
	// searchable torrents by event time and link to Search; source bars show yield by discovery
	// cohort and link to the Discover page. The response carries no shares, so none are shown.
	import { formatCount, formatPercent } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import ChartPanel from './ChartPanel.svelte';
	import { searchHref, stageHref } from './links';
	import LinkedBars, { type BarItem } from './LinkedBars.svelte';
	import type { Overview } from './model';
	import type { InsightsQuery } from './query';

	interface Props {
		segments: Overview['segments'];
		query: InsightsQuery;
		rangeText: string;
	}
	let { segments, query, rangeText }: Props = $props();

	const locale = $derived(currentLocale());
	const groupNames: Record<string, () => string> = {
		video: () => m.insights_group_video(),
		audio: () => m.insights_group_audio(),
		documents: () => m.insights_group_documents(),
		software: () => m.insights_group_software(),
		images: () => m.insights_group_images(),
		other: () => m.insights_group_other(),
		adult: () => m.insights_group_adult(),
		unclassified: () => m.insights_group_unclassified()
	};
	const typeNames: Record<string, () => string> = {
		movie: () => m.insights_type_movie(),
		tv: () => m.insights_type_tv(),
		video: () => m.insights_type_video(),
		music: () => m.insights_type_music(),
		audiobook: () => m.insights_type_audiobook(),
		ebook: () => m.insights_type_ebook(),
		comic: () => m.insights_type_comic(),
		software: () => m.insights_type_software(),
		game: () => m.insights_type_game(),
		images: () => m.insights_type_images(),
		other: () => m.insights_type_other(),
		adult: () => m.insights_type_adult(),
		unclassified: () => m.insights_group_unclassified()
	};

	// Largest first; ties by key, so the order is stable between refetches.
	const byCount = <T extends { key: string; searchable: number }>(list: readonly T[]) =>
		[...list].sort((a, b) => b.searchable - a.searchable || a.key.localeCompare(b.key));

	function counts(
		kind: 'group' | 'type',
		names: Record<string, () => string>,
		list: Overview['segments']['by_group']
	) {
		const sorted = byCount(list);
		const scale = Math.max(0, ...sorted.map((s) => s.searchable));
		const items: BarItem[] = sorted.map((s) => {
			const href = searchHref(kind, s.key, query);
			return {
				key: s.key,
				label: names[s.key]?.() ?? s.key,
				href,
				value: s.searchable,
				text: formatCount(s.searchable, locale)
			};
		});
		return { items, scale };
	}
	const group = $derived(counts('group', groupNames, segments.by_group));
	const type = $derived(counts('type', typeNames, segments.by_content_type));

	const sources = $derived(
		[...segments.by_source].sort(
			(a, b) => b.searchable - a.searchable || a.key.localeCompare(b.key)
		)
	);
	const sourceItems = $derived<BarItem[]>(
		sources.map((s) => ({
			key: s.key,
			label: s.key,
			href: stageHref('discover', query),
			value: s.yield,
			text: formatPercent(s.yield, locale, 1),
			sub: m.insights_overview_source_counts({
				searchable: formatCount(s.searchable, locale),
				harvested: formatCount(s.harvested, locale)
			})
		}))
	);

	const countCols = [
		{ key: 'label', label: m.insights_overview_col_segment() },
		{ key: 'count', label: m.insights_overview_col_searchable(), numeric: true }
	];
	const countRows = (items: BarItem[]) => items.map((i) => ({ label: i.label, count: i.text }));
</script>

<p class="ins-summary" id="ins-segments-note" data-testid="insights-segments-note">
	{m.insights_overview_segments_note()}
</p>

<div class="ins-pair">
	<ChartPanel
		id="ins-by-group"
		title={m.insights_overview_by_group()}
		summary={m.insights_overview_by_group_summary({ range: rangeText })}
		caption={m.insights_overview_by_group()}
		columns={countCols}
		rows={countRows(group.items)}
		csv="segments.by_group"
		{query}
		testid="insights-by-group-panel"
	>
		<LinkedBars
			items={group.items}
			max={group.scale}
			label={m.insights_overview_by_group()}
			describedby="ins-segments-note"
			testid="insights-group"
		/>
	</ChartPanel>

	<ChartPanel
		id="ins-by-type"
		title={m.insights_overview_by_type()}
		summary={m.insights_overview_by_type_summary({ range: rangeText })}
		caption={m.insights_overview_by_type()}
		columns={countCols}
		rows={countRows(type.items)}
		csv="segments.by_content_type"
		{query}
		testid="insights-by-type-panel"
	>
		<LinkedBars
			items={type.items}
			max={type.scale}
			label={m.insights_overview_by_type()}
			describedby="ins-segments-note"
			testid="insights-type"
		/>
	</ChartPanel>
</div>

<ChartPanel
	id="ins-by-source"
	title={m.insights_overview_by_source()}
	summary={m.insights_overview_by_source_summary({ range: rangeText })}
	caption={m.insights_overview_by_source()}
	columns={[
		{ key: 'label', label: m.insights_overview_col_source() },
		{ key: 'yield', label: m.insights_overview_kpi_yield(), numeric: true },
		{ key: 'harvested', label: m.harvested(), numeric: true },
		{ key: 'searchable', label: m.searchable(), numeric: true }
	]}
	rows={sources.map((s) => ({
		label: s.key,
		yield: formatPercent(s.yield, locale, 1),
		harvested: formatCount(s.harvested, locale),
		searchable: formatCount(s.searchable, locale)
	}))}
	csv="segments.by_source"
	{query}
	testid="insights-by-source-panel"
>
	<LinkedBars
		items={sourceItems}
		max={1}
		label={m.insights_overview_by_source()}
		testid="insights-source"
	/>
</ChartPanel>
