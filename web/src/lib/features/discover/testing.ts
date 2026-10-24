// Fixtures for Discover's component tests.
import type { components } from '../../api/schema';

type HitGroup = components['schemas']['HitGroup'];
type NewestSection = components['schemas']['NewestSection'];
type SearchHit = components['schemas']['SearchHit'];
export type DiscoverResponse = components['schemas']['DiscoverResponse'];

export const hash = (n: number) => n.toString(16).padStart(40, '0');

export function hit(n: number, over: Partial<SearchHit> = {}): SearchHit {
	return {
		adult_kind: null,
		completed: 3,
		composition: [
			{ group: 'video', bytes: 800, files: 1, pct_bytes: 82, pct_files: 10 },
			{ group: 'audio', bytes: 100, files: 2, pct_bytes: 11, pct_files: 20 }
		],
		content_type: 'movie',
		discovered_at: 1_700_000_000 + n,
		files: 3,
		flags: [],
		hash: hash(n),
		hash_v2: null,
		id: n,
		junk_reasons: [],
		junk_score: 0,
		last_scrape_at: 1_700_000_100,
		last_seen_at: 1_700_000_090,
		leechers: 4,
		metadata_at: 1_700_000_050,
		name: `Raw.Name.${n}.2160p`,
		piece_len: 262144,
		resolved_via: null,
		score: 0,
		seeders: 12,
		seeders_approx: false,
		swarm_suspect: false,
		size: 4_700_000_000,
		title: `Title ${n}`,
		trackers_checked: 20,
		trackers_reporting: 4,
		...over
	};
}

export function newest(over: Partial<NewestSection> = {}): NewestSection {
	const hits = over.hits ?? [hit(1), hit(2)];
	return {
		hits,
		limit: 50,
		next_cursor: null,
		notices: [],
		offset: 0,
		total: hits.length,
		total_approx: false,
		type: null,
		...over
	};
}

export const group = (over: Partial<HitGroup> = {}): HitGroup => ({
	hits: [],
	reason: null,
	status: 'available',
	truncated: false,
	...over
});

export const unavailable = (reason: NonNullable<HitGroup['reason']>): HitGroup =>
	group({ status: 'unavailable', reason });

export function response(over: Partial<DiscoverResponse> = {}): DiscoverResponse {
	return {
		newest: newest(),
		rising: unavailable('needs_swarm_history'),
		recently_resolved: unavailable('needs_resolution_history'),
		...over
	};
}

/** A populated answer for route smoke tests: a few rows, both side sections unavailable. */
export function discoverResponse(over: Partial<DiscoverResponse> = {}): DiscoverResponse {
	const hits = [1, 2, 3].map((n) => hit(n, n === 2 ? { seeders: null, leechers: null } : {}));
	return response({ newest: newest({ hits, total: 3 }), ...over });
}

/** `GET /settings/content` with "Show adult content" set to `on`. */
export function showAdultSettings(on: boolean): components['schemas']['SectionValues'] {
	return {
		fields: {
			show_adult: {
				env: null,
				read_only: false,
				restart_required: false,
				source: 'default',
				value: on
			}
		},
		restart_required: [],
		revision: 1,
		section: 'content'
	};
}
