<script lang="ts">
	import Drawer from '../Drawer.svelte';
	import Tooltip from '../Tooltip.svelte';
	import Tree from '../../lists/Tree.svelte';
	let {
		onWidthChange = () => {},
		width = 560
	}: { onWidthChange?: (w: number) => void; width?: number } = $props();
	let open = $state(false);
	const nodes = [
		{ id: 'd', depth: 0, name: 'Season 1', dir: true },
		{ id: 'd/a', depth: 1, name: 'a.mkv', dir: false }
	];
</script>

<button type="button" data-testid="row" onclick={() => (open = true)}>Row</button>
<Drawer
	bind:open
	title="Ubuntu 24.04"
	closeLabel="Close"
	resizeLabel="Resize"
	{width}
	{onWidthChange}
>
	<button type="button">Primary action</button>
	<Tooltip content="Copies the magnet" delayDuration={0}>
		{#snippet trigger({ props })}<button {...props} type="button">Copy</button>{/snippet}
	</Tooltip>
	<Tree {nodes} label="Files" expanded={new Set(['d'])} />
</Drawer>
