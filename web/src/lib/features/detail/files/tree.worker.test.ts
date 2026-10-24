import { describe, expect, it } from 'vitest';
import type { BuildOptions } from './build';
import { createTreeBuilder } from './tree.worker';

const options: BuildOptions = {
	showPadding: false,
	filter: '',
	matched: [],
	locale: 'en',
	labels: { executable: 'executable', symlink: 'link' }
};
const f = (path: string, size = 1) => ({ path, size, attrs: [] });

describe('tree worker handler', () => {
	it('accumulates appended files and answers a build with nodes, expansion and typed arrays', () => {
		const b = createTreeBuilder();
		expect(b.handle({ type: 'append', files: [f('a/x'), f('a/y')] })).toBeNull();
		expect(b.handle({ type: 'append', files: [f('b')] })).toBeNull();
		const out = b.handle({ type: 'build', seq: 7, options: { ...options, matched: [1] } })!;
		expect(out.reply.seq).toBe(7);
		expect(out.reply.nodes.map((n) => n.id)).toEqual(['a/', 'a/x', 'a/y', 'b']);
		expect(out.reply.expanded).toEqual(['a/']);
		expect(Array.from(out.reply.index.parent)).toEqual([-1, 0, 0, -1]);
		expect(out.reply.index.idToIndex.size).toBe(0);
		// the five typed arrays are transferred, not copied
		expect(out.transfer).toHaveLength(5);
		expect(out.transfer.every((buffer) => buffer instanceof ArrayBuffer)).toBe(true);
	});

	it('re-runs the filter over what it holds', () => {
		const b = createTreeBuilder();
		b.handle({ type: 'append', files: [f('a/x'), f('b/y')] });
		const out = b.handle({ type: 'build', seq: 1, options: { ...options, filter: 'y' } })!;
		expect(out.reply.nodes.map((n) => n.id)).toEqual(['b/', 'b/y']);
	});

	it('forgets everything on reset', () => {
		const b = createTreeBuilder();
		b.handle({ type: 'append', files: [f('a')] });
		b.handle({ type: 'reset' });
		expect(b.handle({ type: 'build', seq: 2, options })!.reply.nodes).toEqual([]);
	});
});
