<script lang="ts">
	import type { HTMLInputAttributes } from 'svelte/elements';
	import { FOCUS_RING } from '../classes';

	interface Props extends Omit<HTMLInputAttributes, 'type' | 'checked' | 'children'> {
		label: string;
		checked?: boolean;
	}

	let { label, checked = $bindable(false), class: className = '', ...rest }: Props = $props();
</script>

<label
	class="inline-flex cursor-pointer items-center gap-2 text-body pointer-coarse:min-h-(--target-min) {className}"
>
	<!-- A checkbox with the switch role: space toggles it, the state is announced as on/off. -->
	<input
		type="checkbox"
		role="switch"
		bind:checked
		class="toggle border-field-border {FOCUS_RING}"
		{...rest}
	/>
	<span>{label}</span>
</label>
