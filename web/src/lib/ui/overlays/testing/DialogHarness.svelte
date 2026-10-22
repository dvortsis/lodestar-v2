<script lang="ts">
	import Dialog from '../Dialog.svelte';
	import Sheet from '../Sheet.svelte';
	let { sheet = false, withTrigger = true }: { sheet?: boolean; withTrigger?: boolean } = $props();
	let open = $state(false);
</script>

<button type="button" onclick={() => (open = true)}>Open programmatically</button>
{#if sheet}
	<Sheet bind:open title="Filters" closeLabel="Close" side="bottom">
		{#snippet trigger({ props })}<button {...props} type="button">Open sheet</button>{/snippet}
		<input aria-label="Field" />
	</Sheet>
{:else if withTrigger}
	<Dialog bind:open title="Confirm" closeLabel="Close" description="Are you sure?">
		{#snippet trigger({ props })}<button {...props} type="button">Open dialog</button>{/snippet}
		<input aria-label="Name" />
		{#snippet footer()}<button type="button" onclick={() => (open = false)}>Done</button>{/snippet}
	</Dialog>
{:else}
	<!-- No trigger prop at all: opened from code only. -->
	<Dialog bind:open title="Confirm" closeLabel="Close" description="Are you sure?">
		<input aria-label="Name" />
	</Dialog>
{/if}
<button type="button">Outside</button>
