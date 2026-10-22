<script lang="ts">
	import type { HTMLInputAttributes } from 'svelte/elements';
	import { FOCUS_RING } from '../classes';

	interface Props extends Omit<HTMLInputAttributes, 'type' | 'checked' | 'children'> {
		label: string;
		checked?: boolean;
		/** Tri-state "some selected" display; it is cleared when the box is toggled. */
		indeterminate?: boolean;
	}

	let {
		label,
		checked = $bindable(false),
		indeterminate = $bindable(false),
		class: className = '',
		...rest
	}: Props = $props();
</script>

<label
	class="inline-flex cursor-pointer items-center gap-2 text-body pointer-coarse:min-h-(--target-min) {className}"
>
	<input
		type="checkbox"
		bind:checked
		bind:indeterminate
		class="checkbox border-field-border {FOCUS_RING}"
		{...rest}
	/>
	<span>{label}</span>
</label>
