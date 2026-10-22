<script lang="ts">
	import { formatCount, formatPercent, type MetricValue } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { lengthPct, numeric, type ValueFormatter } from './shared';

	interface Item {
		key: string;
		label: string;
		value: MetricValue;
		/** Server-computed share as a 0–1 ratio. Omit to hide the percentage. */
		share?: MetricValue;
	}
	interface Props {
		items: Item[];
		/** Accessible name of the list. */
		label: string;
		format?: ValueFormatter;
		/** Shared scale: the largest value across lists being compared. Defaults to this list's. */
		max?: number;
		testid?: string;
	}
	let { items, label, format = formatCount, max, testid = 'hbar-list' }: Props = $props();

	const locale = $derived(currentLocale());
	// Deterministic: value descending, unknowns last, ties by label then key.
	const sorted = $derived(
		[...items].sort((a, b) => {
			const x = numeric(a.value);
			const y = numeric(b.value);
			if (x === null || y === null) return x === y ? 0 : x === null ? 1 : -1;
			return y - x || a.label.localeCompare(b.label, locale) || a.key.localeCompare(b.key);
		})
	);
	const scale = $derived(max ?? Math.max(0, ...items.map((i) => numeric(i.value) ?? 0)));
</script>

<ul class="flex flex-col gap-2" aria-label={label} data-testid={testid}>
	{#each sorted as item (item.key)}
		<li class="flex flex-col gap-1" data-testid="{testid}-item">
			<div class="flex items-baseline justify-between gap-3 text-small">
				<span class="min-w-0 truncate">{item.label}</span>
				<span class="shrink-0 tabular-nums">
					<span data-testid="{testid}-value">{format(item.value, locale)}</span>
					{#if item.share !== undefined}
						<span class="text-muted-text" data-testid="{testid}-share"
							>{formatPercent(item.share, locale, 0)}</span
						>
					{/if}
				</span>
			</div>
			<div class="h-2 rounded-selector bg-selected" aria-hidden="true">
				<div
					class="h-full rounded-selector"
					style:width={lengthPct(numeric(item.value), scale)}
					style:background-color="var(--data-1)"
				></div>
			</div>
		</li>
	{/each}
</ul>
