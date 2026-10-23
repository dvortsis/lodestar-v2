import { describe, expect, it } from 'vitest';
import { buildNodes, type BuildOptions, type FileInput } from './build';
import { buildIndex } from '$lib/ui/lists/tree-model';

const file = (path: string, size = 1, attrs: string[] = []): FileInput => ({ path, size, attrs });
const options = (over: Partial<BuildOptions> = {}): BuildOptions => ({
	showPadding: false,
	filter: '',
	matched: [],
	locale: 'en',
	labels: { executable: 'executable', symlink: 'link' },
	...over
});
const ids = (files: FileInput[], over?: Partial<BuildOptions>) =>
	buildNodes(files, options(over)).nodes.map((n) => `${n.depth}:${n.id}`);

describe('buildNodes', () => {
	it('nests by path with directories before files, each in numeric-aware order', () => {
		const files = [
			file('b/ep10.mkv'),
			file('b/ep2.mkv'),
			file('readme.txt'),
			file('a/x.nfo'),
			file('zeta.txt')
		];
		expect(ids(files)).toEqual([
			'0:a/',
			'1:a/x.nfo',
			'0:b/',
			'1:b/ep2.mkv',
			'1:b/ep10.mkv',
			'0:readme.txt',
			'0:zeta.txt'
		]);
	});

	it('gives folders the exact integer sum of their files, formatted once', () => {
		const files = [file('d/a', 1000), file('d/e/b', 24), file('c', 5)];
		const { nodes } = buildNodes(files, options());
		const byId = Object.fromEntries(nodes.map((n) => [n.id, n.size]));
		// 1000 + 24 = 1024 B -> 1.00 KiB; the nested folder has only 24 B.
		expect(byId['d/']).toBe('1.00 KiB');
		expect(byId['d/e/']).toBe('24 B');
		expect(byId['c']).toBe('5 B');
	});

	it('keeps big sums exact (no float drift)', () => {
		const big = 2 ** 40 + 1;
		const { nodes } = buildNodes([file('x/a', big), file('x/b', big)], options());
		expect(nodes[0]!.size).toBe('2.00 TiB');
	});

	it('hides padding files by default and shows them on request', () => {
		const files = [file('a.mkv', 10), file('.pad/1', 5, ['padding'])];
		expect(ids(files)).toEqual(['0:a.mkv']);
		expect(ids(files, { showPadding: true })).toEqual(['0:.pad/', '1:.pad/1', '0:a.mkv']);
	});

	it('excludes hidden padding from folder sizes', () => {
		const files = [file('d/a', 1000), file('d/p', 24, ['padding'])];
		expect(buildNodes(files, options()).nodes[0]!.size).toBe('1,000 B');
		expect(buildNodes(files, options({ showPadding: true })).nodes[0]!.size).toBe('1.00 KiB');
	});

	it('marks matched files by their position in the torrent order and opens their ancestors', () => {
		const files = [file('a/b/c.mkv'), file('a/d.mkv'), file('z/e.mkv')];
		const { nodes, expanded } = buildNodes(files, options({ matched: [0] }));
		expect(nodes.filter((n) => n.marked).map((n) => n.id)).toEqual(['a/b/c.mkv']);
		expect(expanded.sort()).toEqual(['a/', 'a/b/']);
	});

	it('counts matched positions against the full list, padding included', () => {
		const files = [file('.pad/1', 1, ['padding']), file('a.mkv'), file('b.mkv')];
		const { nodes } = buildNodes(files, options({ matched: [2] }));
		expect(nodes.filter((n) => n.marked).map((n) => n.id)).toEqual(['b.mkv']);
	});

	it('filters by a case-insensitive substring of the path, keeping the ancestors', () => {
		const files = [file('Show/Season 1/Pilot.MKV'), file('Show/extras/bts.mp4'), file('other.txt')];
		const { nodes, expanded } = buildNodes(files, options({ filter: 'pilot' }));
		expect(nodes.map((n) => n.id)).toEqual(['Show/', 'Show/Season 1/', 'Show/Season 1/Pilot.MKV']);
		expect(expanded.sort()).toEqual(['Show/', 'Show/Season 1/']);
		// matches on directory names too
		expect(ids(files, { filter: 'EXTRAS' })).toEqual([
			'0:Show/',
			'1:Show/extras/',
			'2:Show/extras/bts.mp4'
		]);
		expect(ids(files, { filter: 'nothing' })).toEqual([]);
	});

	it('keeps folder sizes whole while filtering', () => {
		const files = [file('d/a', 100), file('d/b', 200)];
		const { nodes } = buildNodes(files, options({ filter: 'a' }));
		expect(nodes.map((n) => n.id)).toEqual(['d/', 'd/a']);
		expect(nodes[0]!.size).toBe('300 B');
	});

	it('tolerates duplicate, empty and leading-slash paths', () => {
		const files = [
			file('/lead/x.txt', 1),
			file('lead//x.txt', 2),
			file('', 3),
			file('///', 4),
			file('same', 5),
			file('same', 6)
		];
		const { nodes } = buildNodes(files, options());
		const unique = new Set(nodes.map((n) => n.id));
		expect(unique.size).toBe(nodes.length);
		expect(nodes.filter((n) => !n.dir).map((n) => n.name)).toEqual([
			'x.txt',
			'x.txt',
			'same',
			'same'
		]);
		expect(nodes[0]).toMatchObject({ id: 'lead/', dir: true, depth: 0 });
	});

	it('keeps a file and a folder of the same name apart', () => {
		const files = [file('a'), file('a/b')];
		const { nodes } = buildNodes(files, options());
		expect(nodes.map((n) => n.id)).toEqual(['a/', 'a/b', 'a']);
	});

	it('handles CJK names and orders them with the locale', () => {
		const files = [file('音楽/第10話.mkv'), file('音楽/第2話.mkv'), file('資料/メモ.txt')];
		const { nodes } = buildNodes(files, options({ locale: 'ja' }));
		expect(nodes.map((n) => n.name)).toEqual([
			'音楽',
			'第2話.mkv',
			'第10話.mkv',
			'資料',
			'メモ.txt'
		]);
	});

	it('shows executable and symlink as text next to the size', () => {
		const { nodes } = buildNodes(
			[file('run.sh', 10, ['executable']), file('l', 0, ['symlink', 'hidden'])],
			options()
		);
		expect(nodes.find((n) => n.id === 'run.sh')!.size).toBe('executable · 10 B');
		expect(nodes.find((n) => n.id === 'l')!.size).toBe('link · 0 B');
	});

	it('produces a tree the Tree index accepts (parents, set sizes)', () => {
		const { nodes } = buildNodes([file('a/b/c'), file('a/d'), file('e')], options());
		const index = buildIndex(nodes);
		expect(Array.from(index.parent)).toEqual([-1, 0, 1, 0, -1]);
		expect(index.leaves[0]).toBe(2);
	});

	it('builds 50,000 files in under a second', () => {
		const files: FileInput[] = [];
		for (let i = 0; i < 50_000; i++) files.push(file(`d${i % 200}/sub${i % 17}/f${i}.bin`, i));
		const t = performance.now();
		const { nodes } = buildNodes(files, options());
		buildIndex(nodes);
		expect(performance.now() - t).toBeLessThan(1000);
		expect(nodes.filter((n) => !n.dir)).toHaveLength(50_000);
	});
});
