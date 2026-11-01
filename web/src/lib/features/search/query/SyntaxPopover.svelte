<script lang="ts">
	// The Syntax cheat sheet (§10.4): the examples from docs/query-language.md "Start here", each
	// with a "Try" that sets and submits it. Loaded on first open. The full guide (/help/syntax)
	// has no slice yet, so there is no link to it.
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import Button from '$lib/ui/controls/Button.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import Popover from '$lib/ui/overlays/Popover.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { EXAMPLES } from './examples';

	interface Props {
		open?: boolean;
		ontry: (query: string) => void;
	}
	let { open = $bindable(false), ontry }: Props = $props();

	function tryIt(query: string) {
		open = false;
		ontry(query);
	}
</script>

<Popover label={m.search_query_syntax_title()} bind:open align="end">
	{#snippet trigger({ props })}
		<button type="button" {...props} class="btn btn-ghost {FOCUS_RING} {TARGET_MIN}"
			>{m.search_query_syntax()}</button
		>
	{/snippet}
	<div class="flex max-w-md min-w-0 flex-col gap-3">
		<p class="text-body">{m.search_query_syntax_lead()}</p>
		<ul class="flex flex-col gap-2">
			{#each EXAMPLES as ex (ex.query)}
				<li class="flex items-center justify-between gap-3">
					<span class="flex min-w-0 flex-col">
						<span class="text-small text-muted-text">{ex.goal()}</span>
						<Mono class="truncate">{ex.query}</Mono>
					</span>
					<Button
						variant="quiet"
						dense
						aria-label={m.search_query_try_label({ query: ex.query })}
						onclick={() => tryIt(ex.query)}>{m.search_query_try()}</Button
					>
				</li>
			{/each}
		</ul>
	</div>
</Popover>
