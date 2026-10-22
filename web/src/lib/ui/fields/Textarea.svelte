<script lang="ts">
	import type { HTMLTextareaAttributes } from 'svelte/elements';
	import { FOCUS_RING } from '../classes';

	interface Props extends HTMLTextareaAttributes {
		value?: string | null;
		mono?: boolean;
	}

	let { value = $bindable(), mono = false, class: className = '', ...rest }: Props = $props();
</script>

<textarea
	bind:value
	class="textarea w-full border-field-border aria-invalid:border-error {mono
		? 'font-mono'
		: ''} {FOCUS_RING} {className}"
	{...rest}></textarea>
