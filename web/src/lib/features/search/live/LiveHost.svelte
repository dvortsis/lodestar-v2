<script lang="ts">
	// "12 new results · Show" (§10.4 "Live"): the poller and its banner, loaded by the results panel
	// once there are rows. The announcement goes through the panel's own polite region.
	import { tick } from 'svelte';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { search } from '$lib/state/search.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Info from '~icons/ph/info-bold';
	import { selection } from '../bulk/selection.svelte';
	import { formatCount } from '../results/ResultRow.svelte';
	import { results, type ResultsQuery } from '../results/results.svelte';
	import { createLive } from './live.svelte';
	import './live.css';

	// `said` is the panel's polite region: cleared first so the same text is read again.
	// `list` is the panel's navigable list (its `moveTo` keeps the active row in step with focus).
	let {
		said = $bindable(''),
		list
	}: { said?: string; list: () => { moveTo: (i: number) => void } | undefined } = $props();
	let frame = 0;
	const announce = (text: string) => {
		said = '';
		cancelAnimationFrame(frame);
		frame = requestAnimationFrame(() => (said = text));
	};
	$effect(() => () => cancelAnimationFrame(frame));
	const locale = $derived(currentLocale());
	const grid = () => document.querySelector<HTMLElement>('#results [role="grid"]');

	const queryShown = (): ResultsQuery => ({
		ast: search.q ? search.ast : search.browse ? { op: 'all' } : null,
		sort: search.sort,
		hidden: search.hidden,
		exact: search.exact,
		browse: search.browse
	});
	const live = createLive({
		shown: () => {
			const q = queryShown();
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
	// The query now shown is the one registered for live counts (the old registration is released).
	$effect(() => {
		void results.shown;
		live.reset();
		const s = results.status === 'ready' ? queryShown() : null;
		if (s?.ast && results.showing(s)) void live.follow(s);
	});
	/** Show: the list again from the top; the list moves its own active row and focus there. */
	async function show() {
		await results.reload();
		live.reset();
		void live.follow(queryShown(), true); // the count starts again from 0
		const g = grid();
		if (g) g.scrollTop = 0;
		await tick();
		list()?.moveTo(0);
	}
</script>

<!-- Not a live region: the throttled announcement (the panel's) is the only thing spoken. -->
{#if live.fresh > 0}
	<div class="lv-banner rp-banner">
		<Info class="lv-icon" aria-hidden="true" />
		<span class="lv-text" data-testid="results-live"
			>{live.fresh === 1
				? m.search_live_new_one()
				: m.search_live_new_other({ count: formatCount(live.fresh, locale) })}</span
		>
		<Button
			dense
			data-testid="results-live-show"
			aria-label={m.search_live_show_label({ count: formatCount(live.fresh, locale) })}
			onclick={() => void show()}>{m.search_live_show()}</Button
		>
	</div>
{/if}
