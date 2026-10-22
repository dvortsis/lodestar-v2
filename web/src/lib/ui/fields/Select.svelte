<script lang="ts" module>
	export interface SelectOption {
		value: string;
		label: string;
		disabled?: boolean;
	}
</script>

<script lang="ts">
	import type { HTMLSelectAttributes } from 'svelte/elements';
	import { FOCUS_RING, TARGET_MIN } from '../classes';

	interface Props extends Omit<HTMLSelectAttributes, 'children' | 'value'> {
		options: SelectOption[];
		value?: string;
		dense?: boolean;
		/** Text of a leading empty option, e.g. "Choose…". Omit for none. */
		placeholder?: string;
	}

	let {
		options,
		value = $bindable(),
		dense = false,
		placeholder,
		class: className = '',
		...rest
	}: Props = $props();
</script>

<!-- A native select: best mobile behaviour, free keyboard and type-ahead support. -->
<select
	bind:value
	class="select w-full border-field-border aria-invalid:border-error {dense
		? 'select-sm'
		: ''} {FOCUS_RING} {TARGET_MIN} {className}"
	{...rest}
>
	{#if placeholder !== undefined}
		<option value="">{placeholder}</option>
	{/if}
	{#each options as option (option.value)}
		<option value={option.value} disabled={option.disabled}>{option.label}</option>
	{/each}
</select>
