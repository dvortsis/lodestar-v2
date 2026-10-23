// Typed sample bodies for the detail tests and the dev harness.
import type { SearchHit, TorrentDetail } from './cache';

export const HASH = '0123456789abcdef0123456789abcdef01234567';

export function torrentDetail(over: Partial<TorrentDetail> = {}): TorrentDetail {
	return {
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
		flags: [],
		hash: HASH,
		hash_v2: null,
		id: 1,
		last_scrape_at: 1_700_100_000,
		leechers: 4,
		magnet: `magnet:?xt=urn:btih:${HASH}`,
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
	};
}

export function searchHit(over: Partial<SearchHit> = {}): SearchHit {
	return {
		adult_kind: null,
		completed: 12,
		composition: [
			{ group: 'video', files: 2, bytes: 2_900_000_000, pct_bytes: 99, pct_files: 67 },
			{ group: 'documents', files: 1, bytes: 2048, pct_bytes: 1, pct_files: 33 }
		],
		content_type: 'tv',
		discovered_at: 1_700_000_000,
		files: 3,
		flags: [],
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
	} as SearchHit;
}
