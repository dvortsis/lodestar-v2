<script lang="ts">
	// Mount once near the app root and pass `shared` to each Tooltip: hovering from one tooltip to
	// the next then skips the open delay. Without it every Tooltip wraps its own provider.
	import { Tooltip } from 'bits-ui';
	import type { Snippet } from 'svelte';
	let { children, delayDuration = 300 }: { children: Snippet; delayDuration?: number } = $props();
</script>

<Tooltip.Provider {delayDuration}>{@render children()}</Tooltip.Provider>
