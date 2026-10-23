<script lang="ts">
	// Kit loads this node with every route (it is the error boundary), so it stays a stub: the screen
	// itself (illustrations included) is fetched only when something has actually failed.
	let View = $state<typeof import('$lib/shell/ErrorView.svelte').default>();
	void import('$lib/shell/ErrorView.svelte').then((mod) => (View = mod.default));
</script>

{#if View}<View />{/if}
