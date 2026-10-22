<script lang="ts">
	import Drawer from '../Drawer.svelte';
	let {
		onWidthChange = () => {},
		width = 560
	}: { onWidthChange?: (w: number) => void; width?: number } = $props();
	let open = $state(false);
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
</Drawer>
