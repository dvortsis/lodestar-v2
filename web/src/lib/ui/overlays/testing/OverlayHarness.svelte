<script lang="ts">
	import Tooltip from '../Tooltip.svelte';
	import Popover from '../Popover.svelte';
	import Menu from '../Menu.svelte';
	let { onSelect = () => {} }: { onSelect?: (id: string) => void } = $props();
</script>

<Tooltip content="Copies the magnet link" delayDuration={0}>
	{#snippet trigger({ props })}<button {...props} type="button">Copy</button>{/snippet}
</Tooltip>
<Popover label="Columns">
	{#snippet trigger({ props })}<button {...props} type="button">Pop</button>{/snippet}
	<input aria-label="Inside popover" />
</Popover>
<Menu
	label="Actions"
	{onSelect}
	items={[
		{ id: 'fav', label: 'Favorite' },
		{ id: 'hide', label: 'Hide' },
		{ id: 'labels', label: 'Labels' },
		{ id: 'off', label: 'Disabled one', disabled: true },
		{ id: 'fix', label: 'Fix parse', separatorBefore: true }
	]}
>
	{#snippet trigger({ props })}<button {...props} type="button">Actions</button>{/snippet}
</Menu>
<button type="button">Outside</button>
