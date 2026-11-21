// Query field labels (X-labels), for the Builder.
import { m } from '$lib/paraglide/messages.js';
import { word } from './label-shared';

// The query fields (`crates/search/src/query.rs` FIELDS), by the name the Builder lists: the
// glossary's words where it has them, the filter chips' labels otherwise.
const FIELD: Record<string, () => string> = {
	name: m.raw_name,
	path: m.search_filters_path,
	size: m.search_filters_size,
	files: m.search_filters_files,
	largest: m.search_filters_largest,
	piece: m.search_filters_piece,
	discovered: m.discovered,
	ext: m.search_filters_ext,
	private: m.search_filters_flag_private,
	v2: m.search_filters_flag_v2,
	hybrid: m.search_filters_flag_hybrid,
	imported: m.search_filters_flag_imported,
	backfill: m.backfill,
	partial: m.search_filters_flag_partial,
	hash: m.search_filters_hash,
	magnet: m.label_field_magnet,
	title: m.resolved_title,
	type: m.content_type,
	res: m.search_filters_res,
	hdr: m.search_filters_hdr,
	codec: m.search_filters_codec,
	audio: m.search_filters_audio,
	lang: m.search_filters_lang,
	group: m.search_filters_group,
	has: m.search_filters_has,
	resolved: m.search_filters_resolved,
	adult: m.adult_kind,
	ambiguous: m.search_filters_flag_ambiguous,
	hidden: m.hidden_by_filters,
	demoted: m.search_filters_flag_demoted,
	flagged: m.search_filters_flag_flagged,
	year: m.search_filters_year,
	s: m.search_filters_season,
	e: m.search_filters_episode,
	junk: m.search_filters_junk_score,
	cat: m.label_field_cat,
	bytes: m.label_field_bytes,
	file: m.label_field_file,
	sig: m.label_field_sig,
	seeders: m.seeders,
	leechers: m.leechers,
	trackers: m.trackers_reporting,
	dht: m.dht_estimate,
	seen: m.last_seen,
	time: m.label_field_time,
	verified: m.label_field_verified,
	source: m.label_field_source,
	network: m.label_field_network
};
/** A query field's label (`res` → "Resolution"); the field's code stays what a query types. */
export const fieldLabel = (name: string): string => word(FIELD, name);
