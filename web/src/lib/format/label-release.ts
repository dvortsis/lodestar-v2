// Release labels (X-labels): the parser's codec, HDR and source names, languages, release groups
// and parsed attributes, for the facets, the Builder and Compare.
import { m } from '$lib/paraglide/messages.js';
import { adultTypeLabel, typeLabel } from './label-type';
import { own, word } from './label-shared';

// The release parser's vocabulary (`crates/release-parser/src/lib.rs`); `labels.test.ts` reads it
// from there and fails when a value has no label.
const VIDEO_CODEC: Record<string, string> = {
	h264: 'H.264',
	h265: 'H.265 (HEVC)',
	av1: 'AV1',
	vp9: 'VP9',
	xvid: 'Xvid',
	divx: 'DivX',
	mpeg2: 'MPEG-2',
	vc1: 'VC-1'
};
/** `h264` → "H.264". */
export const videoCodecLabel = (value: string): string => word(VIDEO_CODEC, value);

const AUDIO_CODEC: Record<string, string> = {
	aac: 'AAC',
	ac3: 'Dolby Digital',
	eac3: 'Dolby Digital Plus',
	dts: 'DTS',
	'dts-hd': 'DTS-HD',
	'dts-hd-ma': 'DTS-HD MA',
	'dts-x': 'DTS:X',
	truehd: 'Dolby TrueHD',
	atmos: 'Dolby Atmos',
	flac: 'FLAC',
	mp3: 'MP3',
	opus: 'Opus',
	lpcm: 'LPCM',
	alac: 'ALAC',
	vorbis: 'Vorbis'
};
/** `dts-hd-ma` → "DTS-HD MA". */
export const audioCodecLabel = (value: string): string => word(AUDIO_CODEC, value);

const HDR: Record<string, string> = {
	hdr: 'HDR',
	hdr10: 'HDR10',
	'hdr10+': 'HDR10+',
	dv: 'Dolby Vision',
	hlg: 'HLG'
};
/** `dv` → "Dolby Vision". */
export const hdrLabel = (value: string): string => word(HDR, value);

// Where a release's video came from. The kinds of copy are words (messages); the rest are names.
const RELEASE_SOURCE: Record<string, string | (() => string)> = {
	cam: m.label_source_cam,
	telesync: m.label_source_telesync,
	telecine: m.label_source_telecine,
	screener: m.label_source_screener,
	workprint: m.label_source_workprint,
	vhs: 'VHS',
	tv: m.label_source_tv,
	dvd: 'DVD',
	hdrip: 'HDRip',
	bluray: 'Blu-ray',
	remux: m.label_source_remux,
	'web-dl': 'WEB-DL',
	webrip: 'WEBRip'
};
/** `web-dl` → "WEB-DL", `cam` → "Camcorder". */
export const releaseSourceLabel = (value: string): string => word(RELEASE_SOURCE, value);

// The language's own name can't be read by the user; one `Intl.DisplayNames` per UI language.
const languageNames = new Map<string, Intl.DisplayNames | null>();
/** An ISO 639-1 code in the UI language (`fr` → "French", 法语), or the code when it has no name. */
export function languageLabel(code: string, locale: string): string {
	if (code === 'multi') return m.label_language_multi();
	let names = languageNames.get(locale);
	if (names === undefined) {
		try {
			names = new Intl.DisplayNames([locale], { type: 'language', fallback: 'none' });
		} catch {
			names = null;
		}
		languageNames.set(locale, names);
	}
	try {
		return names?.of(code) || code;
	} catch {
		// Not a well-formed language tag.
		return code;
	}
}

/** A release group as written in the torrent's name: never re-cased for display. */
export const groupLabel = (value: string): string => value;
/**
 * The value of a query field in plain words (`codec` `h264` → "H.264"): what the facets and the
 * Builder show. Fields with no table (`res`, `ext`, `has`) show the value as sent.
 */
export function valueLabel(field: string, value: string, locale: string): string {
	switch (field) {
		case 'type':
			return typeLabel(value);
		case 'adult':
			return adultTypeLabel(value);
		case 'codec':
			return videoCodecLabel(value);
		case 'audio':
			return audioCodecLabel(value);
		case 'hdr':
			return hdrLabel(value);
		case 'lang':
			return languageLabel(value, locale);
		case 'group':
			return groupLabel(value);
		default:
			return value;
	}
}

// A parsed attribute's value (the release parser's `attributes`, as Compare shows them). List
// attributes arrive joined with ", " and are labelled one part at a time.
const ATTRIBUTE: Record<string, (v: string, locale: string) => string> = {
	source: (v) => releaseSourceLabel(v),
	vcodec: (v) => videoCodecLabel(v),
	acodec: (v) => audioCodecLabel(v),
	hdr: (v) => hdrLabel(v),
	lang: languageLabel,
	subs: languageLabel,
	group: (v) => groupLabel(v)
};
/** `acodec` `"dts-hd-ma, truehd"` → "DTS-HD MA, Dolby TrueHD"; other attributes as sent. */
export function attributeLabel(key: string, value: string, locale: string): string {
	const label = own(ATTRIBUTE, key);
	if (!label) return value;
	return value
		.split(', ')
		.map((part) => label(part, locale))
		.join(', ');
}
