<script lang="ts">
	// "12 new results · Show" (§10.4 "Live"): the poller and its banner, loaded by the results panel
	// once there are rows. The announcement goes through the panel's own polite region.
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { search } from '$lib/state/search.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { selection } from '../bulk/selection.svelte';
	import { results, type ResultsQuery } from '../results/results.svelte';
	import { createLive } from './live.svelte';

	// `said` is the panel's polite region: cleared first so the same text is read again.
	let { said = $bindable('') }: { said?: string } = $props();
	const announce = (text: string) => {
		said = '';
		requestAnimationFrame(() => (said = text));
	};
	const locale = $derived(currentLocale());
	const grid = () => document.querySelector<HTMLElement>('#results [role="grid"]');

	const live = createLive({
		shown: () => {
			const q: ResultsQuery = {
				ast: search.q ? search.ast : search.browse ? { op: 'all' } : null,
				sort: search.sort,
				hidden: search.hidden,
				exact: search.exact,
				browse: search.browse
			};
			return q.ast && results.status === 'ready' && results.showing(q)
				? { query: q, total: results.total, approx: results.totalApprox }
				: null;
		},
		// Scrolled deep, or a selection held (S-search-5): nothing should change under the user.
		paused: () => {
			const g = grid();
			return selection.mode || selection.count > 0 || (!!g && g.scrollTop > g.clientHeight * 2);
		},
		announce: (n) =>
			announce(
				n === 1
					? m.search_live_announce_one()
					: m.search_live_announce_other({ count: formatCount(n, locale) })
			)
	});
	$effect(() => live.start());
	// A new query (or sort) replaces the rows: whatever was counted new is moot.
	$effect(() => {
		void results.shown;
		live.reset();
	});
	/** Show: the list again from the top, with focus on its first row (the button is gone). */
	async function show() {
		live.reset();
		await results.retry();
		const g = grid();
		if (g) g.scrollTop = 0;
		g?.querySelector<HTMLElement>('[data-index="0"]')?.focus({ preventScroll: true });
	}
</script>

{#if live.fresh > 0}
	<Banner kind="info" class="rp-banner">
		<span data-testid="results-live"
			>{live.fresh === 1
				? m.search_live_new_one()
				: m.search_live_new_other({ count: formatCount(live.fresh, locale) })}</span
		>
		{#snippet action()}
			<Button dense data-testid="results-live-show" onclick={() => void show()}
				>{m.search_live_show()}</Button
			>
		{/snippet}
	</Banner>
{/if}
