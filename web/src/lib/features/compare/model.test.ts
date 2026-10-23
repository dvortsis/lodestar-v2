import { describe, expect, it } from 'vitest';
import { attributeKeys, buildRows, compositionView, diffSections } from './model';
import { fileList, response, torrent } from './testing';

describe('attributeKeys', () => {
	it('puts known keys in story order, then unknown keys by key', () => {
		const differs = { zeta: true, group: false, year: false, alpha: true, title: false, res: true };
		expect(attributeKeys(differs)).toEqual(['title', 'year', 'res', 'group', 'alpha', 'zeta']);
	});
});

describe('buildRows', () => {
	const rows = buildRows(response(3), 'en');
	const row = (id: string) => rows.find((r) => r.id === id)!;

	it('is in story order', () => {
		expect(rows.map((r) => r.id)).toEqual([
			'type',
			'attr-title',
			'attr-year',
			'attr-res',
			'attr-group',
			'size',
			'files',
			'seeders',
			'leechers',
			'trackers',
			'discovered',
			'composition'
		]);
	});

	it("marks only what the server's differs says, and never size, files or swarm", () => {
		expect(rows.filter((r) => r.differs).map((r) => r.id)).toEqual(['attr-res', 'attr-group']);
	});

	it('shows nulls as null (the dash), never zero', () => {
		const r = buildRows(
			response(2, {
				torrents: [
					torrent(0, {
						seeders: null,
						leechers: null,
						trackers_reporting: null,
						content_type: null
					}),
					torrent(1, { seeders: 0 })
				]
			}),
			'en'
		);
		const cell = (id: string, i: number) => r.find((x) => x.id === id)!.cells[i]!;
		expect(cell('seeders', 0).text).toBeNull();
		expect(cell('seeders', 0).name).toBe('Never scraped');
		expect(cell('seeders', 1).text).toBe('0');
		expect(cell('trackers', 0).text).toBeNull();
		expect(cell('type', 0).text).toBeNull();
		expect(cell('type', 0).name).toBe('Not classified');
		expect(cell('attr-group', 1).text).toBeNull();
	});

	it('formats through the shared formatters', () => {
		expect(row('size').cells[0]!.text).toBe('4.66\u00a0GiB');
		expect(row('files').cells[1]!.text).toBe('4');
		expect(row('trackers').cells[0]!.text).toBe('4 / 20');
		expect(row('type').cells[0]!.text).toBe('Movie');
	});

	it('names an unknown attribute key as itself, in Mono', () => {
		const r = buildRows(response(2, { differs: { odd_key: true } }), 'en').find(
			(x) => x.id === 'attr-odd_key'
		)!;
		expect(r.label).toBe('odd_key');
		expect(r.mono).toBe(true);
	});
});

describe('compositionView', () => {
	it('is null before classification', () => {
		expect(compositionView(torrent(0, { composition: [] }), 'en')).toBeNull();
	});

	it('summarises every share and names the dominant one', () => {
		const v = compositionView(torrent(0), 'en')!;
		expect(v.summary).toBe('80% Video, 20% Audio');
		expect(v.dominant).toBe('80% Video');
		expect(v.segments.map((s) => s.group)).toEqual(['video', 'audio']);
	});

	it('breaks ties in bar order', () => {
		const v = compositionView(
			torrent(0, {
				composition: [
					{ group: 'audio', bytes: 1, files: 1, pct_bytes: 50, pct_files: 50 },
					{ group: 'video', bytes: 1, files: 1, pct_bytes: 50, pct_files: 50 }
				]
			}),
			'en'
		)!;
		expect(v.dominant).toBe('50% Video');
	});
});

describe('diffSections', () => {
	it('is Only in A, Only in B, In both for two torrents', () => {
		expect(diffSections(response(2)).map((s) => s.title)).toEqual([
			'Only in A',
			'Only in B',
			'In both'
		]);
	});

	it('is Only in <letter> each, then In all, for three to five', () => {
		const s = diffSections(response(5));
		expect(s.map((x) => x.title)).toEqual([
			'Only in A',
			'Only in B',
			'Only in C',
			'Only in D',
			'Only in E',
			'In all'
		]);
		expect(s[0]!.hash).toBe('a'.repeat(40));
		expect(s[5]!.hash).toBeUndefined();
	});

	it('carries the server list untouched', () => {
		const data = response(2);
		data.file_diff.common = fileList(12345, 3, true);
		const common = diffSections(data).at(-1)!;
		expect(common.list.count).toBe(12345);
		expect(common.list.truncated).toBe(true);
	});
});
