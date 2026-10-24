// Test data typed from the generated schema, shared by the results tests.
import type { components } from '$lib/api/schema';
import type { SearchHit } from './ResultRow.svelte';

type Response = components['schemas']['SearchResponse'];
export const T0 = 1_700_000_000;

export const hit = (n: number, over: Partial<SearchHit> = {}): SearchHit => ({
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

export const page = (ids: number[], over: Partial<Response> = {}): Response => ({
	canonical: '',
	hits: ids.map((n) => hit(n)),
	limit: 50,
	next_cursor: null,
	notices: [],
	offset: 0,
	stemmed: false,
	took_ms: 1,
	total: ids.length,
	total_approx: false,
	...over
});
