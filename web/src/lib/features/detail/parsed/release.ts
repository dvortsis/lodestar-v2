import { attributeLabel } from '$lib/format/label-release';
import { m } from '$lib/paraglide/messages.js';

// The Release list (§10.5.3): the parsed attributes of `TorrentDetail.attributes`, one plain label
// per key. A key without a label is dropped, never shown raw. Labels and value words are Compare's
// (`compare_attr_*`, `format/label-release`), so one torrent reads the same in both views (§10.6);
// the four keys Compare has no label for (version, sample, platform, arch) have their own.
const LABELS: Record<string, () => string> = {
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
	sample: () => m.detail_attr_sample(),
	version: () => m.detail_attr_version(),
	platform: () => m.detail_attr_platform(),
	arch: () => m.detail_attr_arch(),
	flags: () => m.compare_attr_flags()
};
// Compare's story order (compare/model.ts ATTRIBUTE_ORDER), with the music, software and platform keys
// before the release flags.
const ORDER = Object.keys(LABELS);

/** The keys Parsed's identity rows show (the title's spans across versions). */
export const SPAN_KEYS = ['year', 'season', 'episode', 'absolute'] as const;

// Streaming services (`lexicon.rs`): names, not words, so they aren't translated.
const SERVICES: Record<string, string> = {
	amzn: 'Amazon',
	nf: 'Netflix',
	dsnp: 'Disney+',
	hmax: 'HBO Max',
	max: 'Max',
	atvp: 'Apple TV+',
	hulu: 'Hulu',
	pcok: 'Peacock',
	pmtp: 'Paramount+',
	cr: 'Crunchyroll',
	ip: 'BBC iPlayer',
	stan: 'Stan',
	crav: 'Crave',
	funi: 'Funimation',
	hidi: 'HIDIVE',
	mubi: 'MUBI'
};

function format(key: string, value: string, locale: string): string {
	switch (key) {
		case 'bits':
			return m.detail_attr_bit_depth({ n: value });
		case 'pack':
			return value === 'yes' ? m.detail_attr_pack_yes() : value;
		case 'service':
			return SERVICES[value] ?? value;
		default:
			return attributeLabel(key, value, locale);
	}
}

/**
 * The rows to show, in `ORDER`. Null, `{}` and unknown keys give none. `hidden` keys are left out
 * (the identity rows show them, or are still loading).
 */
export function releaseRows(
	attributes: Record<string, string> | null,
	locale: string,
	resolvedTitle: string | null | undefined,
	hidden: ReadonlySet<string>
): { key: string; label: string; value: string }[] {
	if (!attributes) return [];
	const out: { key: string; label: string; value: string }[] = [];
	for (const key of ORDER) {
		const value = attributes[key];
		if (!value || hidden.has(key)) continue;
		if (key === 'title' && value === resolvedTitle) continue;
		out.push({ key, label: LABELS[key]!(), value: format(key, value, locale) });
	}
	return out;
}
