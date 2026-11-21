<script lang="ts">
	// The one page frame (§10.2): the side gutters every destination shares, and an optional width
	// cap. The frame is left-aligned, never centred, so every page's heading starts at the same edge
	// at every width; `size` only limits how far the content runs to the right.
	//   full:       the whole width (Search, with its rail and drawer);
	//   wide:       up to 80rem (Alerts, Compare, Discover, Detail);
	//   manuscript: --manuscript-width (the unavailable page).
	// Few, plain props: the frame is on every route's first load, so each dynamic attribute counts
	// against its byte budget (§10.1).
	import type { Snippet } from 'svelte';
	import { PAGE_FRAME, PAGE_FRAME_BODY, PAGE_MANUSCRIPT, PAGE_WIDE } from './classes';

	let { size = 'wide', children }: { size?: 'full' | 'wide' | 'manuscript'; children: Snippet } =
		$props();
</script>

<div class={PAGE_FRAME} data-page-frame="">
	<div
		class="{PAGE_FRAME_BODY} {size === 'wide'
			? PAGE_WIDE
			: size === 'manuscript'
				? PAGE_MANUSCRIPT
				: ''}"
	>
		{@render children()}
	</div>
</div>
