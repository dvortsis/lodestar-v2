import { describe, expect, it } from 'vitest';
import { HASH, searchHit, torrentDetail } from '../fixtures';
import {
	base,
	compareBlock,
	compareSet,
	differs,
	lists,
	type HitGroup,
	type Siblings
} from './lists';

const A = 'a'.repeat(40);
const B = 'b'.repeat(40);

const group = (hits = [] as ReturnType<typeof searchHit>[], truncated = false): HitGroup => ({
	hits,
	reason: null,
	status: 'available',
	truncated
});
const off = (reason: HitGroup['reason']): HitGroup => ({
	hits: [],
	reason,
	status: 'unavailable',
	truncated: false
});
const siblings = (over: Partial<Siblings> = {}): Siblings => ({
	hash: HASH,
	identical_payload: group(),
	identical_payload_partial: false,
	identity: null,
	other_versions: group(),
	same_file: group(),
	same_file_capped: false,
	same_file_partial: false,
	similar_payload: group(),
	similar_payload_partial: false,
	...over
});

describe('lists', () => {
	it('keeps the four lists in the UI plan order', () => {
		expect(lists(siblings(), HASH).map((l) => l.key)).toEqual([
			'other_versions',
			'identical_payload',
			'same_file',
			'similar_payload'
		]);
	});

	it('drops the open torrent and repeats, and keeps what the server sent in order', () => {
		const rows = [
			searchHit({ hash: HASH }),
			searchHit({ hash: B }),
			searchHit({ hash: A }),
			searchHit({ hash: B })
		];
		const [versions] = lists(siblings({ other_versions: group(rows, true) }), HASH);
		expect(versions!.hits.map((h) => h.hash)).toEqual([B, A]);
		expect(versions!.truncated).toBe(true);
	});

	it('marks an unavailable list with its reason and no rows', () => {
		const l = lists(siblings({ same_file: off('needs_v2_files'), same_file_partial: true }), HASH);
		expect(l[2]).toMatchObject({ hits: [], reason: 'needs_v2_files', gap: null, truncated: false });
	});

	it('says when a list may be missing some', () => {
		const l = lists(
			siblings({
				identical_payload_partial: true,
				same_file_capped: true,
				similar_payload_partial: true
			}),
			HASH
		);
		expect(l.map((x) => x.gap)).toEqual([null, 'partial', 'capped', 'partial']);
		// partial wins over capped: the index filling in is the broader reason
		expect(lists(siblings({ same_file_capped: true, same_file_partial: true }), HASH)[2]!.gap).toBe(
			'partial'
		);
	});
});

describe('differs', () => {
	it('names only the attributes that differ from the open torrent', () => {
		const open = base(torrentDetail(), searchHit());
		expect(differs(searchHit({ hash: A }), open)).toEqual([]);
		expect(
			differs(searchHit({ hash: A, resolution: '2160p', size: 9, seeders: null }), open)
		).toEqual(['resolution', 'size', 'seeders']);
		expect(differs(searchHit({ hash: A, title: 'Other' }), open)).toEqual(['title']);
	});

	it("never calls a field different when the open torrent's data doesn't carry it", () => {
		const open = base(torrentDetail({ size: 5, files: 3, seeders: 120 }), null);
		expect(
			differs(searchHit({ hash: A, title: 'Other', resolution: '720p', size: 5 }), open)
		).toEqual([]);
		expect(differs(searchHit({ hash: A, size: 6, files: 4 }), open)).toEqual(['size', 'files']);
	});
});

describe('Compare these', () => {
	it('puts the open torrent first, then the picks in order', () => {
		expect(compareSet(HASH, [B, A])).toEqual([HASH, B, A]);
		expect(compareSet(HASH, [HASH, A])).toEqual([HASH, A]);
	});

	it('runs with 1 to 4 picks (2 to 5 torrents)', () => {
		expect(compareBlock(0)).toBe('none');
		expect([1, 2, 3, 4].map(compareBlock)).toEqual([null, null, null, null]);
		expect(compareBlock(5)).toBe('over');
	});
});
