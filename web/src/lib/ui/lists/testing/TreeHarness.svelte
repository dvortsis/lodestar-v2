<script lang="ts">
	import Tree from '../Tree.svelte';
	import { buildIndex, type FlatNode } from '../tree-model';
	let {
		nodes,
		onChecked,
		precomputed = false
	}: { nodes: FlatNode[]; onChecked?: (c: Set<string>) => void; precomputed?: boolean } = $props();
	// svelte-ignore state_referenced_locally
	const index = precomputed ? buildIndex(nodes) : undefined;
	let expanded = $state(new Set<string>());
	let checked = $state(new Set<string>());
</script>

<Tree
	{nodes}
	label="Files"
	{index}
	{expanded}
	onExpandedChange={(e) => (expanded = e)}
	{checked}
	onCheckedChange={(c) => {
		checked = c;
		onChecked?.(c);
	}}
/>
<span data-testid="checked">{checked.size}</span>
