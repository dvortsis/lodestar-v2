<svelte:options runes />

<script lang="ts">
	// Marks a result whose metadata Lodestar hasn't confirmed: icon + text, never colour alone.
	// `imported` (X-import-verified-no): from another indexer's database, shown by default, so it
	// reads "Imported" in place of "Unverified", never both. Drawn by hand rather than with
	// ui/Badge, which imports every status icon, and in runes mode so the legacy runtime isn't
	// pulled into the size-limited drawer and results chunks.
	import Warning from '~icons/ph/warning-bold';
	import Database from '~icons/ph/database-bold';
	import { m } from '$lib/paraglide/messages.js';

	let { imported = false }: { imported?: boolean } = $props();
</script>

{#if imported}
	<span
		class="inline-flex items-center gap-1 rounded-selector border border-base-300 px-1.5 text-caption text-base-content"
		title={m.search_imported_badge_title()}
		data-testid="imported-badge"
	>
		<Database class="size-4 shrink-0" aria-hidden="true" />
		{m.search_imported_badge()}
	</span>
{:else}
	<span
		class="inline-flex items-center gap-1 rounded-selector border border-warning px-1.5 text-caption text-base-content"
		data-testid="unverified-badge"
	>
		<Warning class="size-4 shrink-0 text-warning" aria-hidden="true" />
		{m.search_unverified_badge()}
	</span>
{/if}
