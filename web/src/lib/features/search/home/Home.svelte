<script lang="ts">
	// The Search empty state, the home screen (§10.3, §10.4; UI review P6, D11): what Lodestar is,
	// what to type, whether it is working, and the person's recent searches (kept in this browser
	// only). Its own chunk, loaded with the results panel, so it is there in the first frame.
	// Everything that arrives late (saved searches) is last, so nothing moves.
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { shellStatus } from '$lib/shell/status.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { clearRecent, loadRecent } from '../assist/recent';
	import { EXAMPLES } from '../query/examples';
	import { searchHref } from '../results/links';
	import type { components } from '$lib/api/schema';

	interface Props {
		/** Focus the query bar. */
		focusBar: () => void;
		/** Searchable torrents; null while unknown. */
		count: number | null;
		saved: components['schemas']['SavedSearchView'][];
	}
	let { focusBar, count, saved }: Props = $props();

	const locale = $derived(currentLocale());
	let recent = $state(loadRecent());

	const kind = $derived(shellStatus.pill.kind);
	const health = $derived(
		kind === 'crawling'
			? m.search_home_health_working()
			: kind === 'idle'
				? m.search_home_health_ready()
				: kind === 'paused'
					? m.search_home_health_paused()
					: kind === 'stalled' || kind === 'failed' || kind === 'restart'
						? m.search_home_health_attention()
						: ''
	);
</script>

<div class="rp-empty" data-home="" data-testid="results-empty">
	<h2 class="rp-empty-title" data-testid="results-empty-title">
		{count !== null
			? m.search_home_title({ count: formatCount(count, locale) })
			: m.search_home_title_unknown()}
	</h2>
	<p>{m.search_home_lead()}</p>
	<!-- Always there, at its own height: the status arrives after the first frame. -->
	<p class="rp-health" data-testid="home-health">{health}</p>
	<ul class="rp-examples">
		{#each EXAMPLES.slice(0, 3) as ex (ex.query)}
			<li>
				<!-- The query last on its line: the mono font arrives late, and growing text there moves
				     nothing after it. -->
				{ex.goal()} ·
				<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- searchHref() resolves -->
				<a class="rp-link" href={searchHref(ex.query)}><code>{ex.query}</code></a>
			</li>
		{/each}
	</ul>
	<Button onclick={focusBar}>{m.search_results_empty_focus()}</Button>
	{#if recent.length}
		<h3 class="rp-empty-sub">{m.search_home_recent()}</h3>
		<ul class="rp-examples" data-testid="home-recent">
			{#each recent as q (q)}
				<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- searchHref() resolves -->
				<li><a class="rp-link" href={searchHref(q)}>{q}</a></li>
			{/each}
		</ul>
		<p class="rp-empty-note">
			{m.search_home_recent_note()}
			<Button
				variant="quiet"
				data-testid="home-recent-clear"
				onclick={() => (clearRecent(), (recent = []))}>{m.search_home_recent_clear()}</Button
			>
		</p>
	{/if}
	<!-- Last: the list arrives with its request, after the rest has painted, and must not push
	     anything down (a layout shift). -->
	{#if saved.length}
		<h3 class="rp-empty-sub">{m.search_home_saved()}</h3>
		<ul class="rp-examples" data-testid="results-saved">
			{#each saved as s (s.id)}
				<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- searchHref() resolves -->
				<li><a class="rp-link" href={searchHref(s.q, s.sort)}>{s.name}</a></li>
			{/each}
		</ul>
	{/if}
</div>
