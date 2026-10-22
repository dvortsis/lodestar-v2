<script lang="ts">
	import Tree from '../Tree.svelte';
	import type { FlatNode } from '../tree-model';
	let { nodes, onChecked }: { nodes: FlatNode[]; onChecked?: (c: Set<string>) => void } = $props();
	let expanded = $state(new Set<string>());
	let checked = $state(new Set<string>());
</script>

<Tree
	{nodes}
	label="Files"
	{expanded}
	onExpandedChange={(e) => (expanded = e)}
	{checked}
	onCheckedChange={(c) => {
		checked = c;
		onChecked?.(c);
	}}
	checkLabel={(n) => `Select ${n.name}`}
/>
<span data-testid="checked">{checked.size}</span>
