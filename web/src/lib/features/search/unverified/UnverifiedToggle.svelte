<script lang="ts">
	// "Show unverified" (SRC-3: hidden unless the query asks). Reads and writes the query's AST, so
	// the strip shows the `verified:` chip and Back undoes it, like any facet click.
	import { m } from '$lib/paraglide/messages.js';
	import { search } from '$lib/state/search.svelte';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import { ensureSearchPath } from '../query/path';
	import { showsUnverified, withUnverified } from './toggle';

	const on = $derived(showsUnverified(search.ast));

	function change(e: Event) {
		const next = (e.currentTarget as HTMLInputElement).checked;
		ensureSearchPath();
		search.setAst(withUnverified(search.ast, next)).catch(() => {
			(e.target as HTMLInputElement).checked = on; // nothing changed: show the real state
		});
	}
</script>

<div data-testid="unverified-toggle">
	<Checkbox
		label={m.search_unverified_toggle()}
		checked={on}
		onchange={change}
		aria-describedby="unverified-hint"
	/>
	<p id="unverified-hint" class="text-caption text-base-content/70">
		{m.search_unverified_hint()}
	</p>
</div>
