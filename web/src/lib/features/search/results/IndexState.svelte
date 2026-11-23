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
		/** Null when `/search/state` failed: a live rebuild still shows. Before it answers, the
		 *  remembered figures (`index-memory.ts`). */
		state: Pick<IndexState, 'searchable_torrents' | 'growth_per_hour'> | null;
		full?: boolean;
		/** Rows are on screen: the growing line may not get longer (it could wrap and move them). */
		hold?: boolean;
		focusBar: () => void;
	}
	// `state` is read as `figures`: a local named `state` would make `$state` a store read.
	let { banner, state: figures, full, hold, focusBar }: Props = $props();
	const locale = $derived(currentLocale());
	// A rate under 1/h would show as "+0/h": leave it out, and a missing one too.
	const per = $derived(figures?.growth_per_hour ?? 0);
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
	// X-index-banner-late: new figures fill the line in place, unless the text gets longer while rows
	// are on screen (a rate appearing, a digit more); that waits for the next moment without rows.
	const text = $derived(growing(figures?.searchable_torrents ?? 0));
	let last = ''; // what the line showed last: a plain variable, so the rule below reads it untracked
	const line = $derived.by(() => {
		if (!hold || !last || text.length <= last.length) last = text;
		return last;
	});
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
{#if full && figures}
	<EmptyState
		title={m.search_state_cold_title()}
		text={rate !== null
			? m.search_state_cold_body_rate({
					count: formatCount(figures.searchable_torrents, locale),
					rate: formatRate(rate, locale)
				})
			: m.search_state_cold_body({ count: formatCount(figures.searchable_torrents, locale) })}
		data-testid="results-cold-page"
		textTestid="results-cold-start"
	>
		{#snippet illustration()}<StateScene id="searching_crawling" />{/snippet}
		{#snippet action()}
			<Button onclick={focusBar}>{m.search_results_empty_focus()}</Button>
		{/snippet}
	</EmptyState>
{:else if banner?.kind === 'growing' && results.status !== 'empty'}
	<Banner kind="info" class="rp-banner rp-growing">
		<span data-testid="results-cold-start">{line}</span>
	</Banner>
{/if}
