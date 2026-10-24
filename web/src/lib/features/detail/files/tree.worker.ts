// Builds the file tree off the main thread for big torrents (§10.1: above 2,000 files). A module
// worker from a same-origin file, so the server's `script-src 'self'` allows it (never a blob: URL).
import { buildIndex, type TreeIndex } from '$lib/ui/lists/tree-model';
import { buildNodes, type BuildOptions, type FileInput } from './build';
import type { FlatNode } from '$lib/ui/lists/tree-model';

export type WorkerRequest =
	| { type: 'reset' }
	| { type: 'append'; files: FileInput[] }
	| { type: 'build'; seq: number; options: BuildOptions };

export interface WorkerResult {
	type: 'result';
	seq: number;
	nodes: FlatNode[];
	expanded: string[];
	index: TreeIndex;
}

/** The worker's whole behaviour as a function, so tests drive it without a Worker. */
export function createTreeBuilder() {
	let files: FileInput[] = [];
	return {
		handle(message: WorkerRequest): { reply: WorkerResult; transfer: ArrayBuffer[] } | null {
			if (message.type === 'reset') {
				files = [];
				return null;
			}
			if (message.type === 'append') {
				for (const f of message.files) files.push(f);
				return null;
			}
			const { nodes, expanded } = buildNodes(files, message.options);
			const full = buildIndex(nodes);
			// The Tree reads only the typed arrays; the id map would cost a large structured clone.
			const index: TreeIndex = { ...full, idToIndex: new Map() };
			const transfer = [full.parent, full.posinset, full.setsize, full.end, full.leaves].map(
				(a) => a.buffer as ArrayBuffer
			);
			return { reply: { type: 'result', seq: message.seq, nodes, expanded, index }, transfer };
		}
	};
}

// Only wire the message loop inside a real worker.
if (typeof window === 'undefined' && typeof self !== 'undefined') {
	const builder = createTreeBuilder();
	self.onmessage = (event: MessageEvent<WorkerRequest>) => {
		const out = builder.handle(event.data);
		if (out) (self as unknown as Worker).postMessage(out.reply, out.transfer);
	};
}
