<script lang="ts">
	// Marks a result whose metadata Lodestar hasn't confirmed: icon + text, never colour alone.
	// Drawn by hand rather than with ui/Badge, which imports every status icon: this one needs one,
	// and the drawer and the results list are size-limited.
	import Warning from '~icons/ph/warning';
	import { m } from '$lib/paraglide/messages.js';
</script>

<span
	class="inline-flex items-center gap-1 rounded-selector border border-warning px-1.5 text-caption text-base-content"
	title={m.search_unverified_badge_title()}
	data-testid="unverified-badge"
>
	<Warning class="size-4 shrink-0 text-warning" aria-hidden="true" />
	{m.search_unverified_badge()}
</span>
