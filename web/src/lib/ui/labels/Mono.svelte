<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		children: Snippet;
		/** Break anywhere (hashes, long paths) instead of overflowing. */
		wrap?: boolean;
		class?: string;
	}

	let { children, wrap = false, class: className = '' }: Props = $props();
</script>

<code class="font-mono text-small {wrap ? 'break-all' : ''} {className}">{@render children()}</code>
