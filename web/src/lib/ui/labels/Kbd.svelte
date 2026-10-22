<script lang="ts">
	import type { Snippet } from 'svelte';

	let { children, class: className = '' }: { children: Snippet; class?: string } = $props();
</script>

<kbd
	class="inline-flex min-w-6 items-center justify-center rounded-selector border border-field-border bg-base-100 px-1 font-mono text-caption text-base-content {className}"
	>{@render children()}</kbd
>
