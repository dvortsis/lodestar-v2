<script lang="ts">
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { FOCUS_RING, TARGET_MIN } from '../classes';

	interface Props {
		/** `null` means unbounded on that side. */
		min: number | null;
		max: number | null;
		/** Group label, e.g. "Size". */
		label: string;
		minLabel: string;
		maxLabel: string;
		/** Unit shown beside the inputs, e.g. "MiB". */
		unit?: string;
		/** Formats the live summary and error; defaults to `formatCount`. */
		format?: (value: number) => string;
		step?: number;
		dense?: boolean;
		/** Message when min > max; the inputs get aria-invalid. */
		invalidMessage?: string;
		onchange?: (range: { min: number | null; max: number | null }) => void;
		class?: string;
	}

	let {
		min = $bindable(null),
		max = $bindable(null),
		label,
		minLabel,
		maxLabel,
		unit,
		format = (v) => formatCount(v, currentLocale()),
		step = 1,
		dense = false,
		invalidMessage,
		onchange,
		class: className = ''
	}: Props = $props();

	const uid = $props.id();
	const invalid = $derived(min !== null && max !== null && min > max);

	function parse(raw: string): number | null {
		if (raw.trim() === '') return null;
		const n = Number(raw);
		return Number.isFinite(n) ? n : null;
	}

	const summary = $derived(
		`${min === null ? '—' : format(min)} ${m.common_to()} ${max === null ? '—' : format(max)}${unit ? ` ${unit}` : ''}`
	);
</script>

<div role="group" aria-label={label} class="flex flex-col gap-1 {className}">
	<div class="flex items-center gap-2">
		{#each [{ key: 'min', text: minLabel }, { key: 'max', text: maxLabel }] as field, i (field.key)}
			{#if i === 1}<span class="text-muted-text" aria-hidden="true">–</span>{/if}
			<input
				type="number"
				inputmode="decimal"
				{step}
				aria-label={field.text}
				aria-invalid={invalid ? true : undefined}
				aria-describedby={invalid && invalidMessage ? `${uid}-error` : undefined}
				value={field.key === 'min' ? (min ?? '') : (max ?? '')}
				class="input w-(--compbar-max-width) border-field-border tabular-nums aria-invalid:border-error {dense
					? 'input-sm'
					: ''} {FOCUS_RING} {TARGET_MIN}"
				oninput={(e) => {
					const n = parse(e.currentTarget.value);
					if (field.key === 'min') min = n;
					else max = n;
					onchange?.({ min, max });
				}}
			/>
		{/each}
		{#if unit}<span class="text-body text-muted-text">{unit}</span>{/if}
	</div>
	<p class="text-small text-muted-text tabular-nums" data-testid="range-summary" aria-live="off">
		{summary}
	</p>
	{#if invalid && invalidMessage}
		<p id="{uid}-error" class="text-small text-base-content" data-testid="field-error">
			{invalidMessage}
		</p>
	{/if}
</div>
