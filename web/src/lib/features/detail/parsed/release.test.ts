import { describe, expect, it } from 'vitest';
import { ATTRIBUTE_ORDER } from '../../compare/model';
import { ORDER } from './release';

// The keys of `TorrentDetail.attributes` (docs/decisions/2026-11-28-X-detail-attributes.md).
const KEYS = [
	'title',
	'alt',
	'year',
	'season',
	'episode',
	'absolute',
	'date',
	'res',
	'source',
	'service',
	'vcodec',
	'bits',
	'hdr',
	'acodec',
	'channels',
	'lang',
	'subs',
	'edition',
	'flags',
	'pack',
	'group',
	'container',
	'artist',
	'album',
	'author',
	'bitrate',
	'sample',
	'version',
	'platform',
	'arch'
];

describe('the Release list keys', () => {
	it('has every key Compare knows, in Compare’s relative order', () => {
		const shared = ORDER.filter((k) => (ATTRIBUTE_ORDER as readonly string[]).includes(k));
		expect(shared).toEqual([...ATTRIBUTE_ORDER]);
	});

	it('labels all 30 keys the server can send, once each', () => {
		expect(new Set(ORDER).size).toBe(ORDER.length);
		expect([...ORDER].sort()).toEqual([...KEYS].sort());
	});
});
