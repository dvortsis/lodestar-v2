// Test data typed from the generated schema, shared by this feature's unit tests.
import type { AlertRow, SavedSearch } from './api';

type Hit = AlertRow['torrent'];
export const T0 = 1_700_000_000;

export const saved = (over: Partial<SavedSearch> = {}): SavedSearch => ({
	alert: true,
	alerts: 0,
	ast: { op: 'all' },
	created_at: T0,
	id: 1,
	last_alert_at: null,
	name: 'Lantern Keeper releases',
	paused: false,
	q: 'lantern keeper',
	rss: false,
	sort: null,
	unread: 0,
	updated_at: T0,
	...over
});

export const hit = (n: number, over: Partial<Hit> = {}): Hit => ({
	adult_kind: null,
	completed: 5,
	composition: [
		{ group: 'video', bytes: 900, files: 2, pct_bytes: 82, pct_files: 40 },
		{ group: 'audio', bytes: 100, files: 3, pct_bytes: 11, pct_files: 60 }
	],
	content_type: 'video',
	discovered_at: T0,
	files: 5,
	flags: [],
	hash: n.toString(16).padStart(40, '0'),
	hash_v2: null,
	id: n,
	junk_reasons: [],
	junk_score: 0,
	last_scrape_at: T0,
	leechers: 3,
	metadata_at: T0,
	name: `Lantern.Keeper.${n}`,
	piece_len: 16384,
	resolved_via: null,
	score: 0,
	seeders: 40,
	seeders_approx: false,
	swarm_suspect: false,
	last_seen_at: T0,
	size: 2_147_483_648,
	title: `Lantern Keeper ${n}`,
	trackers_checked: 20,
	trackers_reporting: 4,
	...over
});

export const row = (id: number, read: boolean, over: Partial<Hit> = {}): AlertRow => ({
	id,
	created_at: T0 + id,
	read,
	torrent: hit(id, over)
});
