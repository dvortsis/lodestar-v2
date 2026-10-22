<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLButtonAttributes } from 'svelte/elements';
	import { FOCUS_RING, TARGET_MIN } from '../classes';

	interface Props extends Omit<HTMLButtonAttributes, 'children' | 'aria-label'> {
		/** Required accessible name (a prop from the feature, so it is translated there). */
		label: string;
		variant?: 'secondary' | 'quiet' | 'danger';
		dense?: boolean;
		/** The icon, `aria-hidden` by the primitive. */
		children: Snippet;
	}

	let {
		label,
		variant = 'quiet',
		dense = false,
		type = 'button',
		class: className = '',
		children,
		...rest
	}: Props = $props();

	const variants = {
		secondary: 'btn-outline border-field-border',
		quiet: 'btn-ghost',
		danger: 'btn-error'
	};
</script>

<button
	{type}
	aria-label={label}
	class="btn {dense ? 'btn-sm btn-square' : 'btn-square'} {variants[
		variant
	]} {FOCUS_RING} {TARGET_MIN} {className}"
	{...rest}
>
	<span class="inline-flex" aria-hidden="true">{@render children()}</span>
</button>
