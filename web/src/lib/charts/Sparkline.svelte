<script lang="ts">
	import { sparkPath } from './sparkline';

	interface Props {
		/** Samples in time order. `null` is a gap, never zero. */
		values: (number | null)[];
		/** Accessible description, e.g. "Seeders over 7 days". */
		label: string;
		testid?: string;
	}
	let { values, label, testid = 'sparkline' }: Props = $props();

	const W = 64;
	const H = 24;
	const PAD = 2;
	const geo = $derived(sparkPath(values, W, H, PAD));
</script>

<svg
	class="h-6 w-(--compbar-min-width) shrink-0 overflow-visible"
	viewBox="0 0 {W} {H}"
	role="img"
	aria-label={label}
	data-testid={testid}
	data-points={geo.points}
>
	{#if geo.d}
		<path
			d={geo.d}
			fill="none"
			stroke="var(--data-1)"
			stroke-width="1.5"
			stroke-linejoin="round"
			stroke-linecap="round"
			vector-effect="non-scaling-stroke"
		/>
	{/if}
	{#each geo.dots as dot (dot.x)}
		<circle cx={dot.x} cy={dot.y} r="1.5" fill="var(--data-1)" />
	{/each}
</svg>
