// Typed bodies for the detail specs and the `torrent` route's mock (support/routes.ts).
import type { components } from '../src/lib/api/schema';
import type { OpenApiMock } from './mock/openapi-mock';

type Detail = components['schemas']['TorrentDetail'];
type Hit = components['schemas']['SearchHit'];
type File = components['schemas']['FileEntry'];

export const HASH = '0123456789abcdef0123456789abcdef00000000';
export const BIG = 'c'.repeat(40);

export const detail = (over: Partial<Detail> = {}): Detail => ({
	completed: 12,
	dht_peers: null,
	dht_seeds: null,
	discovered_at: 1_700_000_000,
	file_list: [
		{ path: 'Show/S01E01.mkv', size: 1_500_000_000, attrs: [] },
		{ path: 'Show/S01E02.mkv', size: 1_400_000_000, attrs: [] },
		{ path: 'Show/readme.txt', size: 2048, attrs: [] }
	],
	file_list_total: 3,
	files: 3,
	flags: ['private'],
	hash: HASH,
	hash_v2: null,
	id: 1,
	last_scrape_at: 1_700_100_000,
	leechers: 4,
	magnet: `magnet:?xt=urn:btih:${HASH}&dn=Show`,
	metadata_at: 1_700_000_100,
	name: 'Show.S01.1080p.WEB-DL-GRP',
	piece_len: 4_194_304,
	seeders: 120,
	size: 2_900_000_000,
	swarm_history: null,
	swarm_trend: null,
	trackers_checked: 20,
	trackers_reporting: 4,
	...over
});

export const hit = (over: Partial<Hit> = {}): Hit => ({
	adult_kind: null,
	completed: 12,
	composition: [
		{ group: 'video', files: 2, bytes: 2_900_000_000, pct_bytes: 99, pct_files: 67 },
		{ group: 'documents', files: 1, bytes: 2048, pct_bytes: 1, pct_files: 33 }
	],
	content_type: 'tv',
	discovered_at: 1_700_000_000,
	files: 3,
	flags: ['private'],
	hash: HASH,
	hash_v2: null,
	id: 1,
	junk_reasons: [],
	junk_score: 0,
	last_scrape_at: 1_700_100_000,
	leechers: 4,
	metadata_at: 1_700_000_100,
	name: 'Show.S01.1080p.WEB-DL-GRP',
	piece_len: 4_194_304,
	resolved_via: null,
	score: 1,
	seeders: 120,
	size: 2_900_000_000,
	title: 'Show',
	trackers_checked: 20,
	trackers_reporting: 4,
	...over
});

export const searchBody = (hits: Hit[]) => ({
	canonical: '',
	hits,
	limit: 1,
	next_cursor: null,
	notices: [],
	offset: 0,
	stemmed: false,
	took_ms: 1,
	total: hits.length,
	total_approx: false
});

export const group = (hits: Hit[] = []) => ({
	hits,
	reason: null,
	status: 'available' as const,
	truncated: false
});

export const siblings = (hash = HASH) => ({
	hash,
	identical_payload: group(),
	identity: {
		absolute: [],
		episodes: [],
		seasons: ['1'],
		title_words: ['show'],
		years: ['2019']
	},
	other_versions: group(),
	same_file: group()
});

/** `n` files in 5 folders, torrent order. */
export const manyFiles = (n: number, from = 0): File[] =>
	Array.from({ length: n }, (_, i) => ({
		path: `Archive/part${(from + i) % 5}/recording-${String(from + i).padStart(5, '0')}.flac`,
		size: 1000 + (from + i),
		attrs: []
	}));

/** Answers the four requests a torrent page makes. */
export function mockTorrent(mock: OpenApiMock, d: Detail = detail(), h: Hit | null = hit()) {
	mock.on('getTorrent', d);
	mock.on('searchGet', searchBody(h ? [h] : []));
	mock.on('getTorrentSiblings', siblings(d.hash));
	mock.on('listTorrentFiles', { files: [], offset: d.file_list.length, total: d.file_list_total });
}
