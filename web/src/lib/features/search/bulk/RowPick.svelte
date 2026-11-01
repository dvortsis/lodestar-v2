<script lang="ts">
	// A row's select checkbox (list view), loaded when select mode first shows. Out of the tab order:
	// the list is one tab stop, and `x` selects the active row.
	import { m } from '$lib/paraglide/messages.js';
	import type { SearchHit } from '../results/ResultRow.svelte';
	import './bulk.css';
	import { pick } from './ops';
	import { selection } from './selection.svelte';

	let { hit, rows }: { hit: SearchHit; rows: readonly string[] } = $props();
</script>

<input
	type="checkbox"
	class="checkbox checkbox-sm rp-pick border-field-border"
	tabindex="-1"
	checked={selection.has(hit.hash)}
	aria-label={m.search_bulk_select_row({ name: hit.title ?? hit.name })}
	data-testid="result-{hit.hash}-select"
	onclick={(e) => pick(e, hit.hash, rows)}
/>
