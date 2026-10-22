<script lang="ts" module>
	export interface SegmentOption {
		value: string;
		label: string;
		disabled?: boolean;
	}
</script>

<script lang="ts">
	import { FOCUS_RING, TARGET_MIN } from '../classes';

	interface Props {
		options: SegmentOption[];
		value: string;
		/** Accessible name of the group. */
		label: string;
		dense?: boolean;
		onchange?: (value: string) => void;
		class?: string;
	}

	let {
		options,
		value = $bindable(),
		label,
		dense = false,
		onchange,
		class: className = ''
	}: Props = $props();

	let buttons: HTMLButtonElement[] = $state([]);

	function select(next: string) {
		if (next === value) return;
		value = next;
		onchange?.(next);
	}

	// Radio-group semantics: one tab stop (the checked radio), arrows move and select (wrapping).
	function onkeydown(event: KeyboardEvent, index: number) {
		const enabled = options.flatMap((o, i) => (o.disabled ? [] : [i]));
		const at = enabled.indexOf(index);
		let next: number | undefined;
		if (event.key === 'ArrowRight' || event.key === 'ArrowDown') {
			next = enabled[(at + 1) % enabled.length];
		} else if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') {
			next = enabled[(at - 1 + enabled.length) % enabled.length];
		} else if (event.key === 'Home') {
			next = enabled[0];
		} else if (event.key === 'End') {
			next = enabled[enabled.length - 1];
		} else return;
		event.preventDefault();
		const target = next === undefined ? undefined : options[next];
		if (!target) return;
		select(target.value);
		buttons[next!]?.focus();
	}

	const tabbable = $derived(
		options.some((o) => o.value === value && !o.disabled)
			? value
			: options.find((o) => !o.disabled)?.value
	);
</script>

<div
	role="radiogroup"
	aria-label={label}
	class="inline-flex gap-0.5 rounded-field border border-field-border bg-base-100 p-0.5 {className}"
>
	{#each options as option, i (option.value)}
		<button
			bind:this={buttons[i]}
			type="button"
			role="radio"
			aria-checked={option.value === value}
			disabled={option.disabled}
			tabindex={option.value === tabbable ? 0 : -1}
			class="rounded-field px-3 text-body transition-colors duration-(--duration-fast) {dense
				? 'h-(--control-height-dense)'
				: 'h-(--control-height)'} {option.value === value
				? 'border border-primary bg-selected font-medium text-base-content'
				: 'border border-transparent text-muted-text hover:bg-hover'} disabled:opacity-60 {FOCUS_RING} {TARGET_MIN}"
			onclick={() => !option.disabled && select(option.value)}
			onkeydown={(e) => onkeydown(e, i)}
		>
			{option.label}
		</button>
	{/each}
</div>
