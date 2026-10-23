<script lang="ts">
	// The paused and starting-up scenes (§10.9.6). Loaded only when one applies, with their art.
	import Button from '$lib/ui/controls/Button.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import StateScene from '$lib/shell/StateScene.svelte';
	import type { CrawlerEvent } from './state.svelte';

	interface Props {
		kind: 'paused' | 'starting';
		busy: boolean;
		onresume: () => void;
		live: CrawlerEvent | undefined;
	}
	let { kind, busy, onresume, live }: Props = $props();
	const locale = $derived(currentLocale());
</script>

{#if kind === 'paused'}
	<section
		class="ins-card ins-scene"
		aria-label={m.insights_overview_paused_title()}
		data-testid="insights-paused"
	>
		<div>
			<h2 class="ins-heading">{m.insights_overview_paused_title()}</h2>
			<p class="ins-sentence">{m.insights_overview_paused_body()}</p>
		</div>
		<Button
			variant="primary"
			loading={busy}
			onclick={onresume}
			data-testid="insights-paused-resume"
		>
			{m.insights_overview_resume()}
		</Button>
		<div class="ins-scene-art">
			<StateScene id="offline_idle" />
		</div>
	</section>
{:else}
	<section
		class="ins-card ins-scene"
		aria-label={m.insights_overview_starting_title()}
		data-testid="insights-starting"
	>
		<div>
			<h2 class="ins-heading">{m.insights_overview_starting_title()}</h2>
			<p class="ins-sentence">{m.insights_overview_starting_body()}</p>
		</div>
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
		<div class="ins-scene-art">
			<StateScene id="searching_crawling" />
		</div>
	</section>
{/if}
