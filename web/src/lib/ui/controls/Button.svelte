<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLButtonAttributes } from 'svelte/elements';
	import IconSpinner from '~icons/ph/circle-notch';
	import { FOCUS_RING, TARGET_MIN } from '../classes';

	type Variant = 'primary' | 'secondary' | 'quiet' | 'danger';

	interface Props extends Omit<HTMLButtonAttributes, 'children'> {
		variant?: Variant;
		/** 32 px control for toolbars and facets; default is 40 px. */
		dense?: boolean;
		/**
		 * Keeps the button's width, sets aria-busy and ignores clicks. It uses aria-disabled rather
		 * than `disabled`, so focus stays where it was while the action runs.
		 */
		loading?: boolean;
		children: Snippet;
	}

	let {
		variant = 'secondary',
		dense = false,
		loading = false,
		disabled = false,
		type = 'button',
		class: className = '',
		onclick,
		children,
		...rest
	}: Props = $props();

	const variants: Record<Variant, string> = {
		primary: 'btn-primary',
		secondary: 'btn-outline border-field-border',
		quiet: 'btn-ghost',
		danger: 'btn-error'
	};
</script>

<button
	{type}
	{disabled}
	aria-busy={loading || undefined}
	aria-disabled={loading || undefined}
	class="btn relative {variants[variant]} {dense
		? 'btn-sm'
		: ''} {FOCUS_RING} {TARGET_MIN} {className}"
	onclick={(event) => {
		if (loading) {
			event.preventDefault();
			return;
		}
		onclick?.(event);
	}}
	{...rest}
>
	<span class="inline-flex items-center gap-2 {loading ? 'opacity-0' : ''}">
		{@render children()}
	</span>
	{#if loading}
		<span class="absolute inset-0 flex items-center justify-center" aria-hidden="true">
			<IconSpinner class="size-5 motion-safe:animate-spin" />
		</span>
	{/if}
</button>
