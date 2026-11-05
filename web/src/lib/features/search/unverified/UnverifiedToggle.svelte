<script lang="ts">
	// "Show unverified" (SRC-3: hidden unless the query asks). Reads and writes the query's AST, so
	// the strip shows the `verified:` chip and Back undoes it, like any facet click. `count` is the
	// server's count of unverified items for the shown query (the `verified` facet's `no`).
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { Ast } from '$lib/state/search.svelte';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import { showsUnverified, withUnverified } from './toggle';

	interface Props {
		ast: Ast | null;
		onchange: (next: Ast) => void;
		count?: { value: number; approx: boolean } | null;
	}
	let { ast, onchange, count = null }: Props = $props();

	const on = $derived(showsUnverified(ast));
	function change(e: Event) {
		onchange(withUnverified(ast, (e.currentTarget as HTMLInputElement).checked));
	}
</script>

<div data-testid="unverified-toggle">
	<Checkbox
		label={m.search_unverified_toggle()}
		checked={on}
		onchange={change}
		aria-describedby="unverified-hint{count ? ' unverified-count' : ''}"
	/>
	{#if count}
		<span class="text-caption" id="unverified-count" data-testid="unverified-count"
			>{formatCount(count, currentLocale())}</span
		>
	{/if}
	<p id="unverified-hint" class="fr-muted">
		{m.search_unverified_hint()}
	</p>
</div>
