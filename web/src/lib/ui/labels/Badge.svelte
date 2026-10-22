<script lang="ts" module>
	export type BadgeVariant = 'neutral' | 'info' | 'success' | 'warning' | 'error' | 'accent';
</script>

<script lang="ts">
	import type { Snippet } from 'svelte';
	import IconInfo from '~icons/ph/info';
	import IconSuccess from '~icons/ph/check-circle';
	import IconWarning from '~icons/ph/warning';
	import IconError from '~icons/ph/x-circle';

	interface Props {
		variant?: BadgeVariant;
		/** Hide the status icon (only for neutral/accent, which carry no status). */
		children: Snippet;
		class?: string;
	}

	let { variant = 'neutral', children, class: className = '' }: Props = $props();

	// Status is icon + text, never colour alone (§10.2). Neutral and accent are plain labels.
	const icons = { info: IconInfo, success: IconSuccess, warning: IconWarning, error: IconError };
	const tone = {
		neutral: 'border-field-border text-base-content',
		accent: 'border-primary bg-selected text-base-content',
		info: 'border-info text-base-content',
		success: 'border-success text-base-content',
		warning: 'border-warning text-base-content',
		error: 'border-error text-base-content'
	};
	const ink = {
		info: 'text-info',
		success: 'text-success',
		warning: 'text-warning',
		error: 'text-error'
	};
	const Icon = $derived(variant in icons ? icons[variant as keyof typeof icons] : null);
</script>

<span
	class="inline-flex items-center gap-1 rounded-selector border px-1.5 text-caption {tone[
		variant
	]} {className}"
>
	{#if Icon}
		<Icon class="size-4 shrink-0 {ink[variant as keyof typeof ink]}" aria-hidden="true" />
	{/if}
	{@render children()}
</span>
