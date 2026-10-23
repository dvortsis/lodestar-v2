// What the two layouts show, built once from the server's response. Nothing here compares,
// counts or divides: `differs`, `scale_bytes`, the file lists and every percentage arrive computed.
import type { components } from '$lib/api/schema';
import type { Group } from '$lib/charts/shared';
import { GROUP_ORDER } from '$lib/charts/shared';
import { formatBytes, formatCount, formatDateTime, formatShare } from '$lib/format';
import { m } from '$lib/paraglide/messages.js';

export type CompareResponse = components['schemas']['CompareResponse'];
export type CompareTorrent = components['schemas']['CompareTorrent'];
export type FileList = components['schemas']['FileList'];

/** What the layouts receive from the page. */
export interface LayoutProps {
	data: CompareResponse;
	onCopy: (hash: string) => void;
	onPrefetch: (hash: string) => void;
	onRemove: (hash: string) => void;
}

export const LETTERS = ['A', 'B', 'C', 'D', 'E'] as const;
export const letter = (i: number): string => LETTERS[i] ?? String(i + 1);

/** Story order for the parsed attributes (§10.6); any other key follows, by key. */
export const ATTRIBUTE_ORDER = [
	'title',
	'year',
	'season',
	'episode',
	'absolute',
	'date',
	'res',
	'source',
	'vcodec',
	'acodec',
	'channels',
	'bits',
	'hdr',
	'lang',
	'subs',
	'container',
	'edition',
	'group',
	'service',
	'pack',
	'artist',
	'album',
	'author',
	'alt',
	'bitrate',
	'flags'
] as const;

export function attributeKeys(differs: Record<string, boolean>): string[] {
	const keys = Object.keys(differs);
	const known = ATTRIBUTE_ORDER.filter((k) => keys.includes(k));
	const rest = keys.filter((k) => !(ATTRIBUTE_ORDER as readonly string[]).includes(k)).sort();
	return [...known, ...rest];
}

/** The resolved title, or the raw name when the torrent has none. */
export const headingOf = (t: CompareTorrent): string => t.title ?? t.name;

const GROUP_LABEL: Record<Group, () => string> = {
	video: () => m.video(),
	audio: () => m.audio(),
	// TODO(S-detail): switch these three and the types to the shared keys once they land.
	images: () => m.compare_group_images(),
	documents: () => m.compare_group_documents(),
	archives: () => m.compare_group_archives(),
	software: () => m.software(),
	other: () => m.other()
};
export const groupLabel = (g: string): string =>
	(GROUP_LABEL as Record<string, (() => string) | undefined>)[g]?.() ?? g;

// TODO(S-detail): the content-type labels move to the shared keys when they land.
const TYPE_LABEL: Record<string, () => string> = {
	movie: () => m.compare_type_movie(),
	tv: () => m.compare_type_tv(),
	music: () => m.compare_type_music(),
	audiobook: () => m.compare_type_audiobook(),
	ebook: () => m.compare_type_ebook(),
	comic: () => m.compare_type_comic(),
	software: () => m.compare_type_software(),
	game: () => m.compare_type_game(),
	images: () => m.compare_type_images(),
	video: () => m.compare_type_video(),
	other: () => m.compare_type_other(),
	adult: () => m.compare_type_adult()
};

export interface CompositionView {
	segments: { group: Group; value: number; label: string }[];
	/** "82% video, 11% audio", every share. */
	summary: string;
	/** "82% video": the highest `pct_bytes`, bar order on ties. */
	dominant: string;
}

export function compositionView(t: CompareTorrent, locale: string): CompositionView | null {
	if (t.composition.length === 0) return null;
	const ordered = [...t.composition].sort(
		(a, b) => GROUP_ORDER.indexOf(a.group as Group) - GROUP_ORDER.indexOf(b.group as Group)
	);
	const text = (c: (typeof ordered)[number]) => ({
		share: formatShare(c.pct_bytes, locale),
		group: groupLabel(c.group)
	});
	let top = ordered[0]!;
	for (const c of ordered) if (c.pct_bytes > top.pct_bytes) top = c;
	return {
		segments: ordered.map((c) => ({
			group: c.group as Group,
			value: c.bytes,
			label: groupLabel(c.group)
		})),
		summary: ordered.map((c) => m.compare_dominant_share(text(c))).join(', '),
		dominant: m.compare_dominant_share(text(top))
	};
}

/** One table cell / one list value. `text` null renders "—" with `name` as its accessible name. */
export interface Cell {
	testid: string;
	text: string | null;
	name?: string;
	composition?: CompositionView | null;
}
export interface Row {
	id: string;
	label: string;
	/** True only where the server's `differs` says so. */
	differs: boolean;
	/** An unknown attribute key is shown as itself, in Mono. */
	mono?: boolean;
	cells: Cell[];
}

export function buildRows(data: CompareResponse, locale: string): Row[] {
	const ts = data.torrents;
	const plain = (
		id: string,
		label: string,
		text: (t: CompareTorrent) => string | null,
		name?: string
	): Row => ({
		id,
		label,
		differs: false,
		cells: ts.map((t, i) => ({ testid: `compare-${id}-${i}`, text: text(t), name }))
	});
	const swarm = (id: string, label: string, v: (t: CompareTorrent) => number | null): Row =>
		plain(id, label, (t) => (v(t) === null ? null : formatCount(v(t), locale)), m.never_scraped());

	const rows: Row[] = [
		plain(
			'type',
			m.content_type(),
			(t) => (t.content_type === null ? null : (TYPE_LABEL[t.content_type]?.() ?? t.content_type)),
			m.compare_not_classified()
		)
	];
	for (const key of attributeKeys(data.differs)) {
		const known = (ATTRIBUTE_ORDER as readonly string[]).includes(key);
		rows.push({
			id: `attr-${key}`,
			label: known ? attributeLabel(key) : key,
			mono: !known,
			differs: data.differs[key] === true,
			cells: ts.map((t, i) => ({
				testid: `compare-attr-${key}-${i}`,
				text: t.attributes[key] ?? null,
				name: m.compare_not_set()
			}))
		});
	}
	rows.push(
		plain('size', m.compare_size(), (t) => formatBytes(t.size, locale)),
		plain('files', m.compare_files(), (t) => formatCount(t.files, locale)),
		swarm('seeders', m.seeders(), (t) => t.seeders),
		swarm('leechers', m.leechers(), (t) => t.leechers),
		plain(
			'trackers',
			m.trackers_reporting(),
			(t) =>
				t.trackers_reporting === null || t.trackers_checked === null
					? null
					: `${formatCount(t.trackers_reporting, locale)} / ${formatCount(t.trackers_checked, locale)}`,
			m.never_scraped()
		),
		plain('discovered', m.discovered(), (t) => formatDateTime(t.discovered_at, locale)),
		{
			id: 'composition',
			label: m.composition(),
			differs: false,
			cells: ts.map((t, i) => ({
				testid: `compare-composition-${i}`,
				text: null,
				composition: compositionView(t, locale)
			}))
		}
	);
	return rows;
}

function attributeLabel(key: string): string {
	const labels = ATTRIBUTE_LABELS as Record<string, (() => string) | undefined>;
	return labels[key]?.() ?? key;
}
const ATTRIBUTE_LABELS = {
	title: () => m.compare_attr_title(),
	year: () => m.compare_attr_year(),
	season: () => m.compare_attr_season(),
	episode: () => m.compare_attr_episode(),
	absolute: () => m.compare_attr_absolute(),
	date: () => m.compare_attr_date(),
	res: () => m.compare_attr_res(),
	source: () => m.compare_attr_source(),
	vcodec: () => m.compare_attr_vcodec(),
	acodec: () => m.compare_attr_acodec(),
	channels: () => m.compare_attr_channels(),
	bits: () => m.compare_attr_bits(),
	hdr: () => m.compare_attr_hdr(),
	lang: () => m.compare_attr_lang(),
	subs: () => m.compare_attr_subs(),
	container: () => m.compare_attr_container(),
	edition: () => m.compare_attr_edition(),
	group: () => m.compare_attr_group(),
	service: () => m.compare_attr_service(),
	pack: () => m.compare_attr_pack(),
	artist: () => m.compare_attr_artist(),
	album: () => m.compare_attr_album(),
	author: () => m.compare_attr_author(),
	alt: () => m.compare_attr_alt(),
	bitrate: () => m.compare_attr_bitrate(),
	flags: () => m.compare_attr_flags()
};

export interface DiffSection {
	id: string;
	title: string;
	list: FileList;
	/** The torrent the list belongs to (for the "open the torrent" link), none for "common". */
	hash?: string;
}

/** Two torrents: Only in A, Only in B, In both. Three to five: Only in <letter> each, In all. */
export function diffSections(data: CompareResponse): DiffSection[] {
	const ts = data.torrents;
	const only = data.file_diff.only.map((list, i) => ({
		id: `only-${i}`,
		title: m.compare_only_in({ letter: letter(i) }),
		list,
		hash: ts[i]?.hash
	}));
	const common = {
		id: 'common',
		title: ts.length === 2 ? m.compare_in_both() : m.compare_in_all(),
		list: data.file_diff.common
	};
	return [...only, common];
}
