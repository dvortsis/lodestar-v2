<script lang="ts">
	import type { HTMLInputAttributes } from 'svelte/elements';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { FOCUS_RING } from '../classes';

	interface Props extends Omit<HTMLInputAttributes, 'type' | 'value' | 'children'> {
		value?: number;
		min?: number;
		max?: number;
		step?: number;
		/** Formats the value for display and aria-valuetext; defaults to `formatCount`. */
		format?: (value: number) => string;
		/** Unit text after the value, e.g. "MiB/s". */
		unit?: string;
	}

	let {
		value = $bindable(0),
		min = 0,
		max = 100,
		step = 1,
		format = (v) => formatCount(v, currentLocale()),
		unit,
		class: className = '',
		...rest
	}: Props = $props();

	const text = $derived(unit ? `${format(value)} ${unit}` : format(value));
</script>

<div class="flex items-center gap-3 {className}">
	<input
		type="range"
		bind:value
		{min}
		{max}
		{step}
		aria-valuetext={text}
		class="range range-primary flex-1 {FOCUS_RING}"
		{...rest}
	/>
	<output
		class="min-w-(--compbar-min-width) text-right text-body tabular-nums"
		data-testid="slider-value">{text}</output
	>
</div>
