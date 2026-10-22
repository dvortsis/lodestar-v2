// The Tree's data model: plain arrays and numbers only (no Svelte, no DOM, no closures), so the
// same functions can run in a Web Worker. The Tree itself runs them on the main thread; the
// file-tree feature moves them into a worker for torrents above ~2k files (§10.5) without
// changing this shape.

/** One node, listed in display (pre-)order. A directory is followed by its descendants. */
export interface FlatNode {
	id: string;
	/** 0 for top-level nodes; a node's parent is the nearest earlier node with depth - 1. */
	depth: number;
	name: string;
	dir: boolean;
	/** Already formatted by `lib/format` (folder sizes included). */
	size?: string;
}

export interface TreeIndex {
	/** Index of each node's parent, or -1. */
	parent: Int32Array;
	/** 1-based position among siblings. */
	posinset: Int32Array;
	/** Number of siblings, itself included. */
	setsize: Int32Array;
	/** Index one past the last descendant (so descendants are (i, end[i])). */
	end: Int32Array;
	/** File (leaf) count under a directory; 1 for a file. */
	leaves: Int32Array;
	idToIndex: Map<string, number>;
}

export function buildIndex(nodes: readonly FlatNode[]): TreeIndex {
	const n = nodes.length;
	const parent = new Int32Array(n).fill(-1);
	const posinset = new Int32Array(n);
	const setsize = new Int32Array(n);
	const end = new Int32Array(n).fill(n);
	const leaves = new Int32Array(n);
	const idToIndex = new Map<string, number>();
	const open: number[] = []; // open[d] = index of the latest node at depth d
	const children = new Map<number, number>(); // parent index -> children seen so far
	for (let i = 0; i < n; i++) {
		const node = nodes[i] as FlatNode;
		// A depth that jumps by more than one is treated as a child of the latest node.
		const d = Math.min(Math.max(node.depth, 0), open.length);
		for (let j = open.length - 1; j >= d; j--) end[open[j] as number] = i;
		open.length = d;
		const p = d > 0 ? (open[d - 1] as number) : -1;
		parent[i] = p;
		const seen = (children.get(p) ?? 0) + 1;
		children.set(p, seen);
		posinset[i] = seen;
		open[d] = i;
		idToIndex.set(node.id, i);
		if (!node.dir) leaves[i] = 1;
	}
	for (let i = 0; i < n; i++) setsize[i] = children.get(parent[i] as number) as number;
	for (let i = n - 1; i > 0; i--) {
		const p = parent[i] as number;
		if (p >= 0) leaves[p] = (leaves[p] as number) + (leaves[i] as number);
	}
	return { parent, posinset, setsize, end, leaves, idToIndex };
}

/** Indices of the visible nodes, given the set of expanded directory ids. */
export function visibleIndices(
	nodes: readonly FlatNode[],
	index: TreeIndex,
	expanded: ReadonlySet<string>
): number[] {
	const out: number[] = [];
	for (let i = 0; i < nodes.length;) {
		out.push(i);
		const node = nodes[i] as FlatNode;
		i = node.dir && !expanded.has(node.id) ? (index.end[i] as number) : i + 1;
	}
	return out;
}

export type CheckState = 'on' | 'off' | 'mixed';

/** Checked file count under every node (a file is 1 when checked), in one reverse pass. */
export function checkedCounts(
	nodes: readonly FlatNode[],
	index: TreeIndex,
	checked: ReadonlySet<string>
): Int32Array {
	const n = nodes.length;
	const counts = new Int32Array(n);
	for (let i = n - 1; i >= 0; i--) {
		const node = nodes[i] as FlatNode;
		if (!node.dir && checked.has(node.id)) counts[i] = 1;
		const p = index.parent[i] as number;
		if (p >= 0) counts[p] = (counts[p] as number) + (counts[i] as number);
	}
	return counts;
}

export function checkState(index: TreeIndex, counts: Int32Array, i: number): CheckState {
	const c = counts[i] as number;
	return c === 0 ? 'off' : c === (index.leaves[i] as number) ? 'on' : 'mixed';
}

/** Toggle a node: a file flips; a directory selects all its files unless they all are already. */
export function toggleChecked(
	nodes: readonly FlatNode[],
	index: TreeIndex,
	checked: ReadonlySet<string>,
	i: number
): Set<string> {
	const next = new Set(checked);
	const node = nodes[i] as FlatNode;
	if (!node.dir) {
		if (!next.delete(node.id)) next.add(node.id);
		return next;
	}
	const counts = checkedCounts(nodes, index, checked);
	const turnOn = checkState(index, counts, i) !== 'on';
	for (let j = i + 1; j < (index.end[i] as number); j++) {
		const d = nodes[j] as FlatNode;
		if (d.dir) continue;
		if (turnOn) next.add(d.id);
		else next.delete(d.id);
	}
	return next;
}
