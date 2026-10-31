<script lang="ts">
	// The index-state banners and the cold-start page (§10.4 States), loaded when `/search/state`
	// answers. `full` is the full-page "still growing" state (< 1,000 torrents, no rows to show).
	import { formatCount, formatRate } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { results } from './results.svelte';
	// The bare page shows no "still growing" line: its figures sit in the empty state (§10.4).
	import type { Banner as Kind, IndexState } from './recover';

	interface Props {
		banner: Kind;
		state: IndexState;
		full?: boolean;
	}
	let { banner, state, full }: Props = $props();
	const locale = $derived(currentLocale());
	const unavailable = $derived(banner?.kind === 'rebuild' ? banner.rebuild.unavailable : []);
</script>

{#if banner?.kind === 'rebuild'}
	<Banner kind="info" class="rp-banner">
		<span data-testid="results-rebuilding"
			>{m.search_state_rebuilding({
				done: formatCount(banner.rebuild.done, locale),
				total: formatCount(banner.rebuild.total, locale)
			})}</span
		>
		{#if unavailable.length}
			<span data-testid="results-unavailable">
				{m.search_state_unavailable({ fields: unavailable.join(', ') })}</span
			>
		{/if}
	</Banner>
{/if}
{#if full}
	<div class="rp-empty" data-testid="results-cold-page">
		<p class="rp-empty-title">{m.search_state_cold_title()}</p>
		<p data-testid="results-cold-start">
			{banner?.kind === 'growing' && banner.perHour !== null
				? m.search_state_growing_rate({
						count: formatCount(state.searchable_torrents, locale),
						rate: formatRate(banner.perHour, locale)
					})
				: m.search_state_growing({ count: formatCount(state.searchable_torrents, locale) })}
		</p>
	</div>
{:else if banner?.kind === 'growing' && results.status !== 'empty'}
	<Banner kind="info" class="rp-banner">
		<span data-testid="results-cold-start"
			>{banner.perHour !== null
				? m.search_state_growing_rate({
						count: formatCount(banner.count, locale),
						rate: formatRate(banner.perHour, locale)
					})
				: m.search_state_growing({ count: formatCount(banner.count, locale) })}</span
		>
	</Banner>
{/if}
