<script lang="ts">
	import type { HTMLInputAttributes } from 'svelte/elements';
	import { FOCUS_RING, TARGET_MIN } from '../classes';

	interface Props extends Omit<HTMLInputAttributes, 'size'> {
		dense?: boolean;
		value?: string | number | null;
		/** Mono for identifiers (hashes, paths). */
		mono?: boolean;
	}

	let {
		dense = false,
		mono = false,
		value = $bindable(),
		class: className = '',
		...rest
	}: Props = $props();
</script>

<input
	bind:value
	class="input w-full border-field-border aria-invalid:border-error {dense ? 'input-sm' : ''} {mono
		? 'font-mono'
		: ''} {FOCUS_RING} {TARGET_MIN} {className}"
	{...rest}
/>
