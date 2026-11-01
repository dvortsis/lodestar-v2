<script lang="ts">
	// The index-state banners and the cold-start page (§10.4 States, §10.19), loaded when the index
	// state or a rebuild needs them. `full` is the full-page "still growing" state (< 1,000 torrents).
	import { formatCount, formatRate } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import StateScene from '$lib/shell/StateScene.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import EmptyState from '$lib/ui/feedback/EmptyState.svelte';
	import { results } from './results.svelte';
	import type { Banner as Kind, IndexState } from './recover';

	interface Props {
		banner: Kind;
		/** Null when `/search/state` failed: a live rebuild still shows. */
		state: IndexState | null;
		full?: boolean;
		focusBar: () => void;
	}
	let { banner, state, full, focusBar }: Props = $props();
	const locale = $derived(currentLocale());
	// A rate under 1/h would show as "+0/h": leave it out, and a missing one too.
	const per = $derived(state?.growth_per_hour ?? 0);
	const rate = $derived(per >= 1 ? per : null);
	const unavailable = $derived(
		banner?.kind === 'rebuild'
			? new Intl.ListFormat(locale, { type: 'conjunction' }).format(banner.rebuild.unavailable)
			: ''
	);
	const growing = (count: number) =>
		rate !== null
			? m.search_state_growing_rate({
					count: formatCount(count, locale),
					rate: formatRate(rate, locale)
				})
			: m.search_state_growing({ count: formatCount(count, locale) });
</script>

{#if banner?.kind === 'rebuild'}
	<Banner kind="info" class="rp-banner">
		<span data-testid="results-rebuilding"
			>{m.search_state_rebuilding({
				done: formatCount(banner.rebuild.done, locale),
				total: formatCount(banner.rebuild.total, locale)
			})}</span
		>
		{#if unavailable}
			<span data-testid="results-unavailable">
				{m.search_state_unavailable({ fields: unavailable })}</span
			>
		{/if}
	</Banner>
{/if}
{#if full && state}
	<EmptyState
		title={m.search_state_cold_title()}
		text={rate !== null
			? m.search_state_cold_body_rate({
					count: formatCount(state.searchable_torrents, locale),
					rate: formatRate(rate, locale)
				})
			: m.search_state_cold_body({ count: formatCount(state.searchable_torrents, locale) })}
		data-testid="results-cold-page"
		textTestid="results-cold-start"
	>
		{#snippet illustration()}<StateScene id="searching_crawling" />{/snippet}
		{#snippet action()}
			<Button onclick={focusBar}>{m.search_results_empty_focus()}</Button>
		{/snippet}
	</EmptyState>
{:else if banner?.kind === 'growing' && results.status !== 'empty'}
	<Banner kind="info" class="rp-banner">
		<span data-testid="results-cold-start">{growing(state?.searchable_torrents ?? 0)}</span>
	</Banner>
{/if}
