// Typed fixtures for the unit tests and the e2e mocks. Not imported by the app.
import type { components } from '../../api/schema';

// From the schema, not './model': the e2e typecheck loads this file without generated messages.
type CompareResponse = components['schemas']['CompareResponse'];
type CompareTorrent = components['schemas']['CompareTorrent'];
type FileList = components['schemas']['FileList'];

export const HASHES = ['a', 'b', 'c', 'd', 'e'].map((c) => c.repeat(40));

const NAMES = [
	'The.Lantern.Keeper.2019.2160p.UHD.BluRay.x265-GRP',
	'The.Lantern.Keeper.2019.1080p.WEB-DL.x264-OTHER',
	'The Lantern Keeper (2019) 720p',
	'The.Lantern.Keeper.2019.480p.DVDRip',
	'The.Lantern.Keeper.2019.Extended.1080p'
];
const RES = ['2160p', '1080p', '720p', '480p', '1080p'];

export function torrent(i: number, over: Partial<CompareTorrent> = {}): CompareTorrent {
	return {
		hash: HASHES[i]!,
		name: NAMES[i]!,
		title: 'The Lantern Keeper',
		content_type: 'movie',
		size: (5 - i) * 1_000_000_000,
		files: 3 + i,
		seeders: 40 - i * 10,
		leechers: i,
		trackers_reporting: 4,
		trackers_checked: 20,
		discovered_at: 1_760_000_000 + i * 86_400,
		composition: [
			{ group: 'video', bytes: (5 - i) * 800_000_000, files: 1, pct_bytes: 80, pct_files: 34 },
			{ group: 'audio', bytes: (5 - i) * 200_000_000, files: 2, pct_bytes: 20, pct_files: 66 }
		],
		attributes: {
			title: 'The Lantern Keeper',
			year: '2019',
			res: RES[i]!,
			group: i === 0 ? 'GRP' : null
		},
		...over
	};
}

export function fileList(count: number, listed = count, truncated = false): FileList {
	return {
		count,
		files: Array.from({ length: listed }, (_, n) => ({
			path: `Disc/part-${n}.mkv`,
			size: 1000 + n
		})),
		truncated
	};
}

export function response(n = 2, over: Partial<CompareResponse> = {}): CompareResponse {
	const torrents = Array.from({ length: n }, (_, i) => torrent(i));
	return {
		torrents,
		differs: { title: false, year: false, res: true, group: true },
		scale_bytes: Math.max(...torrents.map((t) => t.size)),
		file_diff: {
			common: fileList(2),
			only: torrents.map((_, i) => fileList(i + 1))
		},
		...over
	};
}
