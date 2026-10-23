import { describe, expect, it } from 'vitest';
import {
	buildIndex,
	checkedCounts,
	checkState,
	toggleChecked,
	visibleIndices,
	type FlatNode
} from './tree-model';

const dir = (id: string, depth: number): FlatNode => ({ id, depth, name: id, dir: true });
const file = (id: string, depth: number): FlatNode => ({ id, depth, name: id, dir: false });

// a/            0
//   b/          1
//     x         2
//     y         3
//   z           4
// c             5
const nodes = [dir('a', 0), dir('b', 1), file('x', 2), file('y', 2), file('z', 1), file('c', 0)];

describe('tree model', () => {
	const index = buildIndex(nodes);

	it('indexes parents, sibling position and subtree ends', () => {
		expect(Array.from(index.parent)).toEqual([-1, 0, 1, 1, 0, -1]);
		expect(Array.from(index.posinset)).toEqual([1, 1, 1, 2, 2, 2]);
		expect(Array.from(index.setsize)).toEqual([2, 2, 2, 2, 2, 2]);
		expect(Array.from(index.end)).toEqual([5, 4, 3, 4, 5, 6]);
		expect(Array.from(index.leaves)).toEqual([3, 2, 1, 1, 1, 1]);
		expect(index.idToIndex.get('y')).toBe(3);
	});

	it('lists only the visible rows for the expanded set', () => {
		expect(visibleIndices(nodes, index, new Set())).toEqual([0, 5]);
		expect(visibleIndices(nodes, index, new Set(['a']))).toEqual([0, 1, 4, 5]);
		expect(visibleIndices(nodes, index, new Set(['a', 'b']))).toEqual([0, 1, 2, 3, 4, 5]);
		// An expanded directory under a collapsed one stays hidden.
		expect(visibleIndices(nodes, index, new Set(['b']))).toEqual([0, 5]);
	});

	it('computes tri-state parents from checked files', () => {
		const counts = checkedCounts(nodes, index, new Set(['x']));
		expect(checkState(index, counts, 0)).toBe('mixed');
		expect(checkState(index, counts, 1)).toBe('mixed');
		expect(checkState(index, counts, 4)).toBe('off');
		const all = checkedCounts(nodes, index, new Set(['x', 'y']));
		expect(checkState(index, all, 1)).toBe('on');
		expect(checkState(index, all, 0)).toBe('mixed');
	});

	it('toggling a directory selects all its files, then clears them', () => {
		const on = toggleChecked(nodes, index, new Set(['x']), 0);
		expect([...on].sort()).toEqual(['x', 'y', 'z']);
		const off = toggleChecked(nodes, index, on, 0);
		expect(off.size).toBe(0);
		expect([...toggleChecked(nodes, index, new Set(), 4)]).toEqual(['z']);
	});

	it('handles an empty tree and tolerates depth jumps', () => {
		expect(buildIndex([]).parent.length).toBe(0);
		const odd = buildIndex([dir('r', 0), file('deep', 5)]);
		expect(Array.from(odd.parent)).toEqual([-1, 0]);
	});

	it('handles 100k nodes within a generous bound', () => {
		const big: FlatNode[] = [];
		for (let a = 0; a < 100; a++) {
			big.push(dir(`d${a}`, 0));
			for (let b = 0; b < 10; b++) {
				big.push(dir(`d${a}/s${b}`, 1));
				for (let c = 0; c < 99; c++) big.push(file(`d${a}/s${b}/f${c}`, 2));
			}
		}
		expect(big.length).toBe(100_100);
		const t0 = performance.now();
		const idx = buildIndex(big);
		const vis = visibleIndices(big, idx, new Set(['d0', 'd1']));
		const counts = checkedCounts(big, idx, new Set(['d0/s0/f0']));
		toggleChecked(big, idx, new Set(), 0);
		const elapsed = performance.now() - t0;
		expect(vis.length).toBe(98 + 2 * 11);
		expect(checkState(idx, counts, 0)).toBe('mixed');
		expect(elapsed).toBeLessThan(1500);
	});
});
