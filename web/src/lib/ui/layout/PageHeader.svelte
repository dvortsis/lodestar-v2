<script lang="ts">
	// The one page header (§10.2): the page's only `h1`, and whatever the page puts at the end of
	// its row (actions, a status) as children. The `h1` is a script focus target (`tabindex="-1"`,
	// never a tab stop) for pages that move focus to it; give it an `id` to find it. Kept plain: it
	// is on every route's first load, so each option costs bytes (§10.1).
	import type { Snippet } from 'svelte';
	import { FOCUS_RING } from '../classes';
	import { PAGE_H1 } from './classes';

	let { title, id, children }: { title: string; id?: string; children?: Snippet } = $props();
</script>

<header class="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
	<h1 {id} class="{PAGE_H1} {FOCUS_RING}" tabindex="-1">{title}</h1>
	{@render children?.()}
</header>
