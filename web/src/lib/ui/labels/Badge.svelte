<script lang="ts" module>
	export type BadgeVariant = 'neutral' | 'info' | 'success' | 'warning' | 'error' | 'accent';
</script>

<script lang="ts">
	import type { Snippet } from 'svelte';
	import { statusIcons as icons, statusInk as ink, statusBorder } from '../status';

	interface Props {
		variant?: BadgeVariant;
		/** Label text. Status variants add their own icon; neutral and accent are plain. */
		children: Snippet;
		class?: string;
	}

	let { variant = 'neutral', children, class: className = '' }: Props = $props();

	// Status is icon + text, never colour alone (§10.2). Neutral and accent are plain labels.
	const tone = {
		neutral: 'border-field-border',
		accent: 'border-primary bg-selected',
		...statusBorder
	};
	const Icon = $derived(variant in icons ? icons[variant as keyof typeof icons] : null);
</script>

<span
	class="inline-flex items-center gap-1 rounded-selector border px-1.5 text-caption text-base-content {tone[
		variant
	]} {className}"
>
	{#if Icon}
		<Icon class="size-4 shrink-0 {ink[variant as keyof typeof ink]}" aria-hidden="true" />
	{/if}
	{@render children()}
</span>
