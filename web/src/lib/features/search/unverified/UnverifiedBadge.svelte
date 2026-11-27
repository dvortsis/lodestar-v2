<svelte:options runes />

<script lang="ts">
	// Marks a result whose metadata Lodestar hasn't confirmed: icon + text, never colour alone.
	// `imported` (X-import-verified-no): from another indexer's database and shown by default, so
	// it reads "Imported" in place of "Unverified", never both, in the neutral colour. Drawn by hand
	// rather than with ui/Badge, which imports every status icon, and in runes mode so the legacy
	// runtime isn't pulled into the size-limited drawer and results chunks.
	import Warning from '~icons/ph/warning-bold';
	import { m } from '$lib/paraglide/messages.js';

	let { imported = false }: { imported?: boolean } = $props();
	// The help reaches a pointer as the tooltip and everyone else as text read with the label.
	const help = $derived(imported ? m.search_unverified_imported_title() : undefined);
</script>

<span
	class="inline-flex items-center gap-1 rounded-selector border px-1.5 text-caption text-base-content {imported
		? 'border-base-300'
		: 'border-warning'}"
	title={help}
	data-testid={imported ? 'imported-badge' : 'unverified-badge'}
>
	<Warning class="size-4 shrink-0 {imported ? '' : 'text-warning'}" aria-hidden="true" />
	{imported ? m.search_unverified_imported() : m.search_unverified_badge()}
	{#if help}<span class="sr-only">{help}</span>{/if}
</span>
