<script lang="ts">
	import { formatCount, formatDelta, type DeltaKind, type MetricValue } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { numeric, type ValueFormatter } from './shared';
	import Sparkline from './Sparkline.svelte';

	interface Props {
		label: string;
		value: MetricValue;
		/** The metric's formatter from `$lib/format`; counts by default. */
		format?: ValueFormatter;
		/** Change against the comparison period (server-computed). Omit when there is none. */
		delta?: { value: MetricValue; kind?: DeltaKind; text: string };
		sparkline?: { values: (number | null)[]; label: string };
		testid?: string;
	}
	let { label, value, format = formatCount, delta, sparkline, testid = 'kpi' }: Props = $props();

	const locale = $derived(currentLocale());
	const change = $derived(delta ? numeric(delta.value) : null);
	// Direction is never colour alone: icon + signed text + the period text.
	const arrow = $derived(change === null || change === 0 ? 'flat' : change > 0 ? 'up' : 'down');
</script>

<div class="flex min-w-0 flex-col gap-1" data-testid={testid}>
	<span class="text-small text-muted-text">{label}</span>
	<div class="flex items-end justify-between gap-3">
		<span class="text-display font-semibold tabular-nums" data-testid="{testid}-value"
			>{format(value, locale)}</span
		>
		{#if sparkline}<Sparkline {...sparkline} testid="{testid}-sparkline" />{/if}
	</div>
	{#if delta}
		<span class="flex items-center gap-1 text-small tabular-nums" data-testid="{testid}-delta">
			<svg
				class="size-3 shrink-0"
				viewBox="0 0 12 12"
				fill="none"
				stroke="currentColor"
				stroke-width="1.5"
				stroke-linecap="round"
				stroke-linejoin="round"
				aria-hidden="true"
				data-trend={arrow}
			>
				<path
					d={arrow === 'up'
						? 'M6 10V2M2.5 5.5 6 2l3.5 3.5'
						: arrow === 'down'
							? 'M6 2v8M2.5 6.5 6 10l3.5-3.5'
							: 'M2 6h8'}
				/>
			</svg>
			<span>{formatDelta(delta.value, locale, delta.kind ?? 'count')}</span>
			<span class="text-muted-text">{delta.text}</span>
		</span>
	{/if}
</div>
