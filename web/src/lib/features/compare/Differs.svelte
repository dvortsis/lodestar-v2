<script lang="ts">
	import IconDiffers from '~icons/ph/not-equals';
	import { m } from '$lib/paraglide/messages.js';
</script>

<!-- A difference is the icon and the word, never colour alone (§10.6). -->
<span class="cmp-differs" data-testid="compare-differs"
	><IconDiffers aria-hidden="true" class="size-4 shrink-0" />{m.compare_differs()}</span
>

<style>
	.cmp-differs {
		display: inline-flex;
		align-items: center;
		gap: calc(var(--spacing) * 1);
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
		font-weight: var(--font-weight-medium);
	}
</style>
