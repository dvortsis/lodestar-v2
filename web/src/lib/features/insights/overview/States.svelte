<script lang="ts">
	// The paused and starting-up scenes (§10.9.6). Loaded only when one applies, with their art.
	import EmptyState from '$lib/ui/feedback/EmptyState.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import StateScene from '$lib/shell/StateScene.svelte';
	import type { CrawlerEvent } from './state.svelte';

	interface Props {
		kind: 'paused' | 'starting';
		live: CrawlerEvent | undefined;
	}
	let { kind, live }: Props = $props();
	const locale = $derived(currentLocale());
</script>

{#if kind === 'paused'}
	<!-- The context bar keeps the one Resume button (§10.14: one next step), so none here. -->
	<div class="ins-card">
		<EmptyState
			title={m.insights_overview_paused_title()}
			text={m.insights_overview_paused_body()}
			data-testid="insights-paused"
		>
			{#snippet illustration()}<StateScene id="offline_idle" />{/snippet}
		</EmptyState>
	</div>
{:else}
	<div class="ins-card">
		<EmptyState
			title={m.insights_overview_starting_title()}
			text={m.insights_overview_starting_body()}
			data-testid="insights-starting"
		>
			{#snippet illustration()}<StateScene id="searching_crawling" />{/snippet}
			{#snippet action()}
				<dl class="ins-counters">
					<div>
						<dt>{m.harvested()}</dt>
						<dd data-testid="insights-starting-harvested">
							{formatCount(live?.harvested, locale)}
						</dd>
					</div>
					<div>
						<dt>{m.insights_overview_funnel_fetched()}</dt>
						<dd data-testid="insights-starting-fetched">
							{formatCount(live?.fetched, locale)}
						</dd>
					</div>
					<div>
						<dt>{m.insights_overview_funnel_stored()}</dt>
						<dd data-testid="insights-starting-stored">
							{formatCount(live?.stored, locale)}
						</dd>
					</div>
				</dl>
			{/snippet}
		</EmptyState>
	</div>
{/if}
