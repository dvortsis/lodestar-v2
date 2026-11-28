import { m } from '$lib/paraglide/messages.js';

// The Release list (§10.5.3): the parsed attributes of `TorrentDetail.attributes`, one plain label
// per key. A key without a label is dropped, never shown raw. The keys Parsed already shows from the
// title identity (year, season, episode, absolute) are not repeated unless they are all that is known.
const LABELS: Record<string, () => string> = {
	title: () => m.detail_attr_title(),
	alt: () => m.detail_attr_alt(),
	date: () => m.detail_attr_date(),
	res: () => m.detail_attr_res(),
	source: () => m.detail_attr_source(),
	service: () => m.detail_attr_service(),
	vcodec: () => m.detail_attr_vcodec(),
	bits: () => m.detail_attr_bits(),
	hdr: () => m.detail_attr_hdr(),
	acodec: () => m.detail_attr_acodec(),
	channels: () => m.detail_attr_channels(),
	lang: () => m.detail_attr_lang(),
	subs: () => m.detail_attr_subs(),
	edition: () => m.detail_attr_edition(),
	flags: () => m.detail_attr_flags(),
	pack: () => m.detail_attr_pack(),
	group: () => m.detail_attr_group(),
	container: () => m.detail_attr_container(),
	artist: () => m.detail_attr_artist(),
	album: () => m.detail_attr_album(),
	author: () => m.detail_attr_author(),
	bitrate: () => m.detail_attr_bitrate(),
	sample: () => m.detail_attr_sample(),
	version: () => m.detail_attr_version(),
	platform: () => m.detail_attr_platform(),
	arch: () => m.detail_attr_arch(),
	year: () => m.search_filters_year(),
	season: () => m.search_filters_season(),
	episode: () => m.detail_attr_episode(),
	absolute: () => m.detail_attr_absolute()
};
const ORDER = Object.keys(LABELS);

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
	cr: 'Crunchyroll'
};

function names(value: string, locale: string): string {
	let dn: Intl.DisplayNames | null = null;
	try {
		dn = new Intl.DisplayNames([locale], { type: 'language' });
	} catch {
		/* unsupported locale: codes show as sent */
	}
	return value
		.split(', ')
		.map((c) => {
			if (c === 'multi') return m.detail_attr_multi();
			try {
				return dn?.of(c) ?? c;
			} catch {
				return c;
			}
		})
		.join(', ');
}

function format(key: string, value: string, locale: string): string {
	switch (key) {
		case 'lang':
		case 'subs':
			return names(value, locale);
		case 'bits':
			return m.detail_attr_bit_depth({ n: value });
		case 'pack':
			return value === 'yes' ? m.detail_attr_pack_yes() : value;
		case 'service':
			return SERVICES[value] ?? value;
		default:
			return value;
	}
}

/** The rows to show, in `ORDER`. Null, `{}` and unknown keys give none. */
export function releaseRows(
	attributes: Record<string, string> | null,
	locale: string,
	resolvedTitle: string | null | undefined,
	identityShown: ReadonlySet<string>
): { key: string; label: string; value: string }[] {
	if (!attributes) return [];
	const out: { key: string; label: string; value: string }[] = [];
	for (const key of ORDER) {
		const value = attributes[key];
		if (!value) continue;
		if (key === 'title' && value === resolvedTitle) continue;
		if (identityShown.has(key)) continue;
		out.push({ key, label: LABELS[key]!(), value: format(key, value, locale) });
	}
	return out;
}
