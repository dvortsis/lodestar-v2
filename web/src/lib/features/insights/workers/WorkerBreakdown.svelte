<script lang="ts">
	// Per-worker tables on the Discover and Fetch pages, and Discover's Marginal gain view. Renders
	// nothing at all when the server left `by_worker` out (a single machine): no heading, no table.
	import DataTableAlternative from '$lib/charts/DataTableAlternative.svelte';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import {
		discoverCols,
		discoverRows,
		fetchCols,
		fetchRows,
		marginalCols,
		marginalRows,
		type DiscoverWorker,
		type FetchWorker
	} from './model';
	import { loadNames } from './names';

	let {
		stage,
		byWorker
	}: { stage: 'discover' | 'fetch'; byWorker: DiscoverWorker[] | FetchWorker[] | undefined } =
		$props();

	const locale = $derived(currentLocale());
	let names = $state.raw(new Map<number, string>());
	const present = $derived(!!byWorker?.length);
	// Names are fetched lazily, once, and only when there is a breakdown to name.
	$effect(() => {
		if (!present) return;
		const ctl = new AbortController();
		void loadNames(ctl.signal).then((n) => {
			if (!ctl.signal.aborted) names = n;
		});
		return () => ctl.abort();
	});
	const discover = $derived(stage === 'discover' ? (byWorker as DiscoverWorker[]) : []);
</script>

{#if present && byWorker}
	<section class="ins-section" aria-labelledby="ins-s-workers" data-testid="insights-workers">
		<h2 id="ins-s-workers" class="ins-heading">{m.insights_workers_title()}</h2>
		<p class="ins-sub">
			{m.insights_workers_note()}
			{#if stage === 'fetch'}{m.insights_workers_note_fetch()}{/if}
		</p>
		<DataTableAlternative
			caption={m.insights_workers_title()}
			columns={stage === 'discover' ? discoverCols() : fetchCols()}
			rows={stage === 'discover'
				? discoverRows(discover, names, locale)
				: fetchRows(byWorker as FetchWorker[], names, locale)}
			visible
			testid="insights-workers-table"
		/>
	</section>
	{#if stage === 'discover'}
		<section class="ins-section" aria-labelledby="ins-s-marginal" data-testid="insights-marginal">
			<h2 id="ins-s-marginal" class="ins-heading">{m.insights_workers_marginal_title()}</h2>
			<p class="ins-sub" data-testid="insights-marginal-definition">
				{m.insights_workers_marginal_definition()}
				{m.insights_workers_marginal_hint()}
			</p>
			<DataTableAlternative
				caption={m.insights_workers_marginal_title()}
				columns={marginalCols()}
				rows={marginalRows(discover, names, locale)}
				visible
				testid="insights-marginal-table"
			/>
		</section>
	{/if}
{/if}
