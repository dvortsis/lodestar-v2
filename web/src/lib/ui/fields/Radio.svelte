<script lang="ts">
	import type { HTMLInputAttributes } from 'svelte/elements';
	import { FOCUS_RING } from '../classes';

	interface Props extends Omit<HTMLInputAttributes, 'type' | 'group' | 'value' | 'children'> {
		label: string;
		/** This radio's value. */
		value: string;
		/** The group's selected value; radios sharing `name` form one group with arrow-key movement. */
		group?: string;
		name: string;
	}

	let { label, value, group = $bindable(), name, class: className = '', ...rest }: Props = $props();
</script>

<label
	class="inline-flex cursor-pointer items-center gap-2 text-body pointer-coarse:min-h-(--target-min) {className}"
>
	<input
		type="radio"
		bind:group
		{name}
		{value}
		class="radio border-field-border {FOCUS_RING}"
		{...rest}
	/>
	<span>{label}</span>
</label>
