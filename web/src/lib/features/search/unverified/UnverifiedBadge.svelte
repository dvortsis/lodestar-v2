<script lang="ts">
	// Marks a result whose metadata Lodestar hasn't confirmed: icon + text, never colour alone.
	import { m } from '$lib/paraglide/messages.js';
	import Badge from '$lib/ui/labels/Badge.svelte';
</script>

<span title={m.search_unverified_badge_title()} data-testid="unverified-badge">
	<Badge variant="warning">{m.search_unverified_badge()}</Badge>
</span>
