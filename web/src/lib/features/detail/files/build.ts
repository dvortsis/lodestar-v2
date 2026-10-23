// Turns the server's decoded file list into the Tree's flat, pre-ordered nodes. Pure: no DOM and no
// Svelte, so the same code runs in the worker (tree.worker.ts) and in the tests.
import { formatBytes } from '$lib/format';
import { showBidi } from '$lib/util/bidi';
import type { FlatNode } from '$lib/ui/lists/tree-model';

export interface FileInput {
	path: string;
	size: number;
	attrs: readonly string[];
}

export interface BuildOptions {
	showPadding: boolean;
	/** Case-insensitive substring of the path. Empty shows everything. */
	filter: string;
	/** Positions in the torrent's file list (`matched_files`). */
	matched: readonly number[];
	locale: string;
	/** Visible text for the `executable` and `symlink` attributes. */
	labels: { executable: string; symlink: string };
}

export interface BuildResult {
	nodes: FlatNode[];
	/** Directories to open: the ancestors of matches (and of filter hits). */
	expanded: string[];
}

interface Dir {
	name: string;
	id: string;
	size: number;
	dirs: Map<string, Dir>;
	files: Leaf[];
	hit: boolean;
}
interface Leaf {
	name: string;
	id: string;
	size: number;
	attrs: readonly string[];
	marked: boolean;
}

const segments = (path: string) => path.split('/').filter((s) => s !== '');

/**
 * Directories sort before files, each by a numeric-aware locale compare (so `ep2` < `ep10`).
 * Directory ids end in `/` and file ids never do, so a file and a folder of one name can't collide;
 * a repeated path gets a numbered id so every key stays unique.
 */
export function buildNodes(files: readonly FileInput[], options: BuildOptions): BuildResult {
	const collator = new Intl.Collator(options.locale, { numeric: true });
	const matched = new Set(options.matched);
	const needle = options.filter.trim().toLowerCase();
	const root: Dir = { name: '', id: '', size: 0, dirs: new Map(), files: [], hit: false };
	const used = new Set<string>();

	for (let pos = 0; pos < files.length; pos++) {
		const file = files[pos] as FileInput;
		if (!options.showPadding && file.attrs.includes('padding')) continue;
		const parts = segments(file.path);
		if (parts.length === 0) continue;
		const path = parts.join('/');
		const hit = needle === '' || path.toLowerCase().includes(needle);
		const ancestors: Dir[] = [root];
		let dir = root;
		for (let d = 0; d < parts.length - 1; d++) {
			const name = parts[d] as string;
			let next = dir.dirs.get(name);
			if (!next) {
				next = { name, id: `${dir.id}${name}/`, size: 0, dirs: new Map(), files: [], hit: false };
				dir.dirs.set(name, next);
			}
			dir = next;
			ancestors.push(dir);
		}
		// Sizes are the exact integer sum of the server's file sizes, whatever the filter hides.
		for (const a of ancestors) a.size += file.size;
		if (!hit) continue;
		for (const a of ancestors) a.hit = true;
		let id = path;
		for (let n = 2; used.has(id); n++) id = `${path}\u0000${n}`;
		used.add(id);
		dir.files.push({
			name: parts[parts.length - 1] as string,
			id,
			size: file.size,
			attrs: file.attrs,
			marked: matched.has(pos)
		});
	}

	const nodes: FlatNode[] = [];
	const expanded: string[] = [];
	const label = (attr: string) =>
		attr === 'executable'
			? options.labels.executable
			: attr === 'symlink'
				? options.labels.symlink
				: '';

	// Returns whether the directory holds a marked file, so its ancestors open by default. With a
	// filter, every directory that has a hit opens, so the hits are visible.
	const walk = (dir: Dir, depth: number): boolean => {
		let anyMarked = false;
		const dirs = [...dir.dirs.values()].filter((d) => d.hit);
		dirs.sort((a, b) => collator.compare(a.name, b.name));
		for (const d of dirs) {
			nodes.push({
				id: d.id,
				depth,
				name: showBidi(d.name),
				dir: true,
				size: formatBytes(d.size, options.locale)
			});
			const inside = walk(d, depth + 1);
			if (inside || needle !== '') expanded.push(d.id);
			anyMarked ||= inside;
		}
		const leaves = [...dir.files].sort((a, b) => collator.compare(a.name, b.name));
		for (const f of leaves) {
			const extras = f.attrs.map(label).filter(Boolean).join(' · ');
			const bytes = formatBytes(f.size, options.locale);
			const node: FlatNode = {
				id: f.id,
				depth,
				name: showBidi(f.name),
				dir: false,
				size: extras ? `${extras} · ${bytes}` : bytes
			};
			if (f.marked) {
				node.marked = true;
				anyMarked = true;
			}
			nodes.push(node);
		}
		return anyMarked;
	};
	walk(root, 0);
	return { nodes, expanded };
}
