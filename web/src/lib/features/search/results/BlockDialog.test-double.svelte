<script lang="ts">
	// Test double for the Block dialog: Apply reports an applied rule and closes.
	let {
		open = $bindable(),
		onapplied,
		onclose
	}: { open: boolean; onapplied: () => void; onclose: () => void } = $props();
</script>

{#if open}
	<button
		data-testid="stub-apply"
		onclick={() => {
			onapplied();
			open = false;
			onclose();
		}}>Apply</button
	>
{/if}
