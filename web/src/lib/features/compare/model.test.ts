import { describe, expect, it } from 'vitest';
import { attributeKeys, buildRows, compositionView, diffSections, isSame, type Row } from './model';
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

	it('shows parsed values in plain words, lists one part at a time (X-labels)', () => {
		const r = buildRows(
			response(2, {
				torrents: [
					torrent(0, {
						attributes: {
							vcodec: 'h265',
							acodec: 'dts-hd-ma, truehd',
							source: 'web-dl',
							lang: 'fr'
						}
					}),
					torrent(1, { attributes: { vcodec: 'x9', group: 'RARBG' } })
				],
				differs: { vcodec: true, acodec: true, source: true, lang: true, group: true }
			}),
			'en'
		);
		const cell = (id: string, i: number) => r.find((x) => x.id === id)!.cells[i]!.text;
		expect(cell('attr-vcodec', 0)).toBe('H.265 (HEVC)');
		expect(cell('attr-acodec', 0)).toBe('DTS-HD MA, Dolby TrueHD');
		expect(cell('attr-source', 0)).toBe('WEB-DL');
		expect(cell('attr-lang', 0)).toBe('French');
		expect(cell('attr-vcodec', 1)).toBe('x9');
		expect(cell('attr-group', 1)).toBe('RARBG');
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
		expect(v.summary).toBe('80% video, 20% audio');
		expect(v.dominant).toBe('80% video');
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
		expect(v.dominant).toBe('50% video');
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

describe('isSame (Differences only)', () => {
	const row = (...texts: (string | null)[]): Row => ({
		id: 'x',
		label: 'X',
		differs: false,
		cells: texts.map((text, i) => ({ testid: `x-${i}`, text }))
	});
	it('two nulls are the same', () => expect(isSame(row(null, null))).toBe(true));
	it('a null against a value differs, in either order', () => {
		expect(isSame(row(null, '1080p'))).toBe(false);
		expect(isSame(row('1080p', null))).toBe(false);
	});
	it('"—" against "—" is the same', () => expect(isSame(row('—', '—'))).toBe(true));
	it('values that format alike are the same', () =>
		expect(isSame(row('1.0 GB', '1.0 GB', '1.0 GB'))).toBe(true));
	it('one odd value out differs', () => expect(isSame(row('a', 'a', 'b'))).toBe(false));
	it('compares composition by its summary', () => {
		const comp = (summary: string): Row['cells'][number] => ({
			testid: summary,
			text: null,
			composition: { segments: [], summary, dominant: summary }
		});
		expect(isSame({ ...row(), cells: [comp('80% video'), comp('80% video')] })).toBe(true);
		expect(isSame({ ...row(), cells: [comp('80% video'), comp('70% video')] })).toBe(false);
	});
});
