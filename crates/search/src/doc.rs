//! A torrent as the index sees it, built from the store's row, file tree
//! and enrichment record.

use lodestar_classify::{Category, Composition, Record};
use lodestar_metainfo::{InfoHash, Text};
use lodestar_store::{FileTree, TorrentRow, flags};
use tantivy::TantivyDocument;
use tantivy::tokenizer::PreTokenizedString;

use crate::query::{Network, TagKey, Verified};
use crate::schema::Fields;
use crate::tokenizer::{self, MAX_NAME_TOKENS};

/// Path tokens per torrent. Huge packs keep their first files' paths; the
/// file index (M3) covers the rest.
pub const MAX_PATH_TOKENS: usize = 20_000;
const MAX_EXT_BYTES: usize = 10;
/// Numbers indexed per span: a `1-1000` episode batch keeps its first
/// 500, a `1900-2020` collection its first 50 years.
const MAX_YEARS: u16 = 50;
const MAX_SEASONS: u16 = 100;
const MAX_EPISODES: u16 = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentDoc {
    pub id: u64,
    pub hashes: Vec<InfoHash>,
    pub name: String,
    /// What the path field indexes: each distinct directory name once,
    /// then every file name (padding files left out). Season packs repeat
    /// their folders in every path; indexing them once keeps the index
    /// within budget (§3).
    pub path_entries: Vec<String>,
    pub exts: Vec<String>,
    pub flags: i64,
    pub size: u64,
    pub files: u64,
    pub largest: u64,
    pub piece_len: u64,
    pub discovered_at: i64,
    pub metadata_at: i64,
    /// What enrichment adds; empty until the torrent is enriched.
    pub enriched: DocEnrichment,
    /// Where it came from (§18.6, SRC-3).
    pub provenance: DocProvenance,
    /// v7. The covered files' size buckets and `{ext}:{bucket}` pairs
    /// ([`crate::files::covered_buckets`]).
    pub file_buckets: Vec<u64>,
    pub file_ext_buckets: Vec<String>,
    /// The payload signature of the non-padding file sizes
    /// ([`lodestar_classify::payload::payload_sig`], the one CS-8a reads),
    /// for `sig:`; `None` with no content files. Generation 1 (GS-0).
    pub payload_sig: Option<u64>,
}

/// The provenance fields of a document (§18.6): `verified`, `network`
/// and `source`. There's no default: each kind of document says what it
/// is, so one built from claims can't be indexed as verified by
/// omission (SRC-3b builds those).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocProvenance {
    pub verified: Verified,
    pub network: Network,
    /// The plugin ids of the sources that found it, as [`source_term`]
    /// gives them.
    pub sources: Vec<String>,
}

impl DocProvenance {
    /// A stored torrent's: verified clearnet BitTorrent metadata
    /// (`torrents` holds nothing else, §18.5), its sources filled in by
    /// the indexer.
    pub fn stored_torrent() -> DocProvenance {
        DocProvenance {
            verified: Verified::Yes,
            network: Network::Bt,
            sources: Vec::new(),
        }
    }
}

/// A source plugin's id as `source:` indexes it, or `None` when it can't
/// be one ([`crate::query::source_value`]): such a source isn't
/// searchable by name. The query parser reads values the same way, so
/// every indexed term can be asked for.
pub fn source_term(plugin: &str) -> Option<String> {
    crate::query::source_value(plugin)
}

/// The enrichment fields of a document (§6, §8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocEnrichment {
    /// The effective title, then the alternative titles.
    pub titles: Vec<String>,
    /// `key:value` terms (see [`TagKey`]).
    pub tags: Vec<String>,
    pub years: Vec<u64>,
    pub seasons: Vec<u64>,
    pub episodes: Vec<u64>,
    pub junk: u64,
    pub ambiguous: bool,
    /// Non-zero shares as (category, % of files, % of bytes).
    pub shares: Vec<(Category, u8, u8)>,
    /// v7. [`title_identity`] of the record.
    pub title_identity: Option<u64>,
}

impl DocEnrichment {
    pub fn new(record: &Record, comp: Option<&Composition>) -> DocEnrichment {
        let mut e = DocEnrichment {
            junk: u64::from(record.junk.score),
            ambiguous: record.ambiguous,
            ..DocEnrichment::default()
        };
        e.titles.extend(record.release("title").map(str::to_owned));
        e.titles
            .extend(record.release_all("alt").map(str::to_owned));
        let mut tag = |key: TagKey, value: &str| {
            let t = format!("{}:{}", key.as_str(), value.to_lowercase());
            if !e.tags.contains(&t) {
                e.tags.push(t);
            }
        };
        tag(TagKey::Type, record.content_type.as_str());
        if let Some(k) = record.adult_kind {
            tag(TagKey::Adult, k.as_str());
        }
        for (key, field) in [
            (TagKey::Res, "res"),
            (TagKey::Hdr, "hdr"),
            (TagKey::Codec, "vcodec"),
            (TagKey::Audio, "acodec"),
            (TagKey::Lang, "lang"),
        ] {
            // Lists are comma-joined in the record.
            for v in record.release_all(field).flat_map(|v| v.split(", ")) {
                tag(key, v);
            }
        }
        if let Some(g) = record.release("group") {
            tag(TagKey::Group, g);
        }
        for name in record.structure.names() {
            tag(TagKey::Has, name);
        }
        if let Some(r) = record.resolved {
            tag(TagKey::Resolved, r.as_str());
        }
        e.years = numbers(record.release_all("year"), MAX_YEARS);
        e.seasons = numbers(record.release_all("season"), MAX_SEASONS);
        e.episodes = numbers(
            record
                .release_all("episode")
                .chain(record.release_all("absolute")),
            MAX_EPISODES,
        );
        e.title_identity = title_identity(record);
        if let Some(c) = comp {
            for cat in Category::ALL {
                let (count, bytes) = (c.pct_count(cat), c.pct_bytes(cat));
                if count > 0 || bytes > 0 {
                    e.shares.push((cat, count, bytes));
                }
            }
        }
        e
    }
}

/// The numbers in rendered spans (`2`, `1-10`), each span capped at
/// `max` numbers, sorted and without repeats.
fn numbers<'a>(spans: impl Iterator<Item = &'a str>, max: u16) -> Vec<u64> {
    let mut out = Vec::new();
    for s in spans {
        let (a, b) = s.split_once('-').unwrap_or((s, s));
        let (Ok(first), Ok(last)) = (a.parse::<u16>(), b.parse::<u16>()) else {
            continue;
        };
        if first > last {
            continue;
        }
        let last = last.min(first.saturating_add(max - 1));
        out.extend((first..=last).map(u64::from));
    }
    out.sort_unstable();
    out.dedup();
    out
}

impl TorrentDoc {
    pub fn new(row: &TorrentRow, tree: &FileTree) -> TorrentDoc {
        let mut dirs: Vec<String> = Vec::new();
        let mut seen_dirs = std::collections::HashSet::new();
        let mut names = Vec::new();
        let mut exts: Vec<String> = Vec::new();
        let mut largest = 0;
        let mut sizes = Vec::with_capacity(tree.files.len());
        for f in tree.files.iter().filter(|f| !f.is_padding()) {
            largest = largest.max(f.size);
            sizes.push(f.size);
            let segments: Vec<String> = f
                .path
                .iter()
                .map(|seg| {
                    Text {
                        raw: seg.clone(),
                        encoding: tree.encoding,
                    }
                    .decode()
                    .into_owned()
                })
                .collect();
            let Some((name, parents)) = segments.split_last() else {
                continue;
            };
            if let Some(ext) = extension(name)
                && !exts.contains(&ext)
            {
                exts.push(ext);
            }
            for d in parents {
                if seen_dirs.insert(d.as_str().to_owned()) {
                    dirs.push(d.clone());
                }
            }
            names.push(name.clone());
        }
        // A single-file torrent's file name is usually its name; still
        // indexed, since names and file names can differ.
        dirs.extend(names);
        let mut hashes = vec![row.info_hash];
        if let Some(v2) = row.info_hash_v2 {
            let short = v2.truncated();
            if short != row.info_hash {
                hashes.push(short);
            }
        }
        let (file_buckets, file_ext_buckets) = crate::files::covered_buckets(tree);
        TorrentDoc {
            id: row.id as u64,
            hashes,
            name: row.name.clone(),
            path_entries: dirs,
            exts,
            flags: row.flags,
            size: row.size,
            files: row.file_count,
            largest,
            piece_len: row.piece_len,
            discovered_at: row.discovered_at,
            metadata_at: row.metadata_at,
            enriched: DocEnrichment::default(),
            provenance: DocProvenance::stored_torrent(),
            file_buckets,
            file_ext_buckets,
            payload_sig: lodestar_classify::payload::payload_sig_of_sizes(sizes),
        }
    }

    pub fn to_document(&self, f: &Fields) -> TantivyDocument {
        let mut d = TantivyDocument::new();
        d.add_u64(f.id, self.id);
        for h in &self.hashes {
            d.add_bytes(f.hash, &h.0);
        }
        d.add_text(f.name, &self.name);
        d.add_pre_tokenized_text(f.path, paths_tokens(&self.path_entries));
        let all: Vec<String> = std::iter::once(self.name.clone())
            .chain(self.path_entries.iter().cloned())
            .collect();
        d.add_pre_tokenized_text(f.text, paths_tokens(&all));
        for e in &self.exts {
            d.add_text(f.ext, e);
        }
        for name in flag_names(self.flags) {
            d.add_text(f.flag, name);
        }
        let e = &self.enriched;
        if e.ambiguous {
            d.add_text(f.flag, "ambiguous");
        }
        for t in &e.titles {
            d.add_text(f.title, t);
        }
        let stems = title_stems(&e.titles);
        if !stems.is_empty() {
            d.add_text(f.title_stem, stems.join(" "));
        }
        for t in &e.tags {
            d.add_text(f.tag, t);
        }
        // The file categories present, for the category facet: one pass
        // over the tag column instead of ten share columns.
        for (cat, count, _) in &e.shares {
            if *count > 0 {
                d.add_text(
                    f.tag,
                    format!("{}:{}", crate::facets::CATEGORY_TAG, cat.as_str()),
                );
            }
        }
        for (field, values) in [
            (f.year, &e.years),
            (f.season, &e.seasons),
            (f.episode, &e.episodes),
        ] {
            for v in values {
                d.add_u64(field, *v);
            }
        }
        if let Some(id) = e.title_identity {
            d.add_u64(f.title_identity, id);
        }
        // `years` is sorted.
        if let Some(y) = e.years.first() {
            d.add_u64(f.release_year, *y);
        }
        d.add_u64(f.junk, e.junk);
        for (cat, count, bytes) in &e.shares {
            if *count > 0 {
                d.add_u64(f.cat_count[cat.index()], u64::from(*count));
            }
            if *bytes > 0 {
                d.add_u64(f.cat_bytes[cat.index()], u64::from(*bytes));
            }
        }
        d.add_u64(f.size, self.size);
        d.add_u64(f.files, self.files);
        d.add_u64(f.largest, self.largest);
        d.add_u64(f.piece_len, self.piece_len);
        d.add_i64(f.discovered, self.discovered_at);
        d.add_i64(f.metadata, self.metadata_at);
        let p = &self.provenance;
        d.add_text(f.verified, p.verified.as_str());
        d.add_text(f.network, p.network.as_str());
        for s in &p.sources {
            d.add_text(f.source, s);
        }
        for b in &self.file_buckets {
            d.add_u64(f.file_bucket, *b);
        }
        for t in &self.file_ext_buckets {
            d.add_text(f.file_ext_bucket, t);
        }
        if let Some(sig) = self.payload_sig {
            d.add_u64(f.payload_sig, sig);
        }
        d
    }
}

/// A torrent's title identity (B13, `docs/decisions/2026-10-22-B13.md`):
/// the effective title's words as search's tokenizer folds them (its
/// parts, so `Spider-Man`, `Spider Man` and `SpiderMan` are all
/// `spider man`), and the sorted, distinct `year`, `season`, `episode`
/// and `absolute` spans as the record renders them, an absent set a value
/// of its own. Two torrents are versions of one work when theirs are
/// equal. The one definition: `api::compare` builds the identity it shows
/// and compares from it, and [`title_identity`] hashes it (B13b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleIdentityParts {
    pub title_words: Vec<String>,
    pub years: Vec<String>,
    pub seasons: Vec<String>,
    pub episodes: Vec<String>,
    pub absolute: Vec<String>,
}

impl TitleIdentityParts {
    /// The identity as one term (v7's `title_identity`): FNV-1a over the
    /// words and each span set in turn, so it's the same in every build.
    /// A collision only over-matches; the parts decide.
    ///
    /// Changing what this hashes, or how, changes every indexed term:
    /// bump `population::GENERATIONS["title_identity"]` in the same
    /// change, so indexes backfill and Siblings use the window meanwhile
    /// instead of narrowing by stale terms (B13b). The pinned values in
    /// `title_identity_follows_b13s_rule` catch such a change.
    pub fn key(&self) -> u64 {
        let mut key = self.title_words.join(" ");
        for spans in [&self.years, &self.seasons, &self.episodes, &self.absolute] {
            key.push('\u{1f}');
            key.push_str(&spans.join(","));
        }
        key.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
    }
}

/// `record`'s [`TitleIdentityParts`]; `None` without a parsed title that
/// has any words.
pub fn title_identity_parts(record: &Record) -> Option<TitleIdentityParts> {
    let title_words: Vec<String> = tokenizer::tokenize(record.release("title")?)
        .into_iter()
        .filter(|t| t.kind == tokenizer::TokKind::Part)
        .map(|t| t.text)
        .collect();
    if title_words.is_empty() {
        return None;
    }
    let spans = |k: &str| {
        let mut v: Vec<String> = record.release_all(k).map(str::to_owned).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    Some(TitleIdentityParts {
        title_words,
        years: spans("year"),
        seasons: spans("season"),
        episodes: spans("episode"),
        absolute: spans("absolute"),
    })
}

/// A torrent's title identity as v7 indexes it: [`TitleIdentityParts::key`]
/// of [`title_identity_parts`], so torrents with one identity share one
/// term. `None` without a title with words.
pub fn title_identity(record: &Record) -> Option<u64> {
    title_identity_parts(record).map(|p| p.key())
}

/// The lowercased extension of a file name, if it looks like one.
pub fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() || ext.len() > MAX_EXT_BYTES {
        return None;
    }
    ext.chars()
        .all(|c| c.is_ascii_alphanumeric())
        .then(|| ext.to_ascii_lowercase())
}

/// Every path entry in one token stream (the field has no positions, so
/// they're only numbered for Tantivy's sake). The text is the entries
/// joined by newlines; offsets point into it.
fn paths_tokens(paths: &[String]) -> PreTokenizedString {
    let mut text = String::new();
    let mut tokens = Vec::new();
    let mut toks = Vec::new();
    let mut position = 0;
    for p in paths {
        if toks.len() >= MAX_PATH_TOKENS {
            break;
        }
        let base = text.len();
        if base > 0 {
            text.push('\n');
        }
        let start = text.len();
        text.push_str(p);
        let before = toks.len();
        let cap = MAX_PATH_TOKENS.min(before + MAX_NAME_TOKENS);
        let next = tokenizer::tokenize_into(p, position, &mut toks, cap);
        for t in &mut toks[before..] {
            t.from += start;
            t.to += start;
        }
        position = next;
    }
    tokens.extend(toks.into_iter().map(tokenizer::to_tantivy));
    PreTokenizedString { text, tokens }
}

/// The `flag:` keywords. Names are part of the query language.
pub fn flag_names(bits: i64) -> impl Iterator<Item = &'static str> {
    const NAMES: &[(i64, &str)] = &[
        (flags::PRIVATE, "private"),
        (flags::V2, "v2"),
        (flags::HYBRID, "hybrid"),
        (flags::PADDING, "padding"),
        (flags::SYMLINKS, "symlinks"),
        (flags::IMPORTED, "imported"),
        (flags::BACKFILL, "backfill"),
        (flags::FILES_PARTIAL, "files_partial"),
        (flags::SINGLE_FILE, "single_file"),
        (flags::WARNINGS, "warnings"),
        (flags::PRIVATE_SOURCE, "private_source"),
        (flags::HIDDEN, "hidden"),
        (flags::DEMOTED, "demoted"),
        (flags::FLAGGED, "flagged"),
    ];
    NAMES
        .iter()
        .filter(move |(bit, _)| bits & bit != 0)
        .map(|(_, n)| *n)
}

/// The distinct light stems of the titles' words, in first-seen order.
fn title_stems(titles: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in titles {
        for tok in crate::tokenizer::tokenize(t) {
            if tok.kind == crate::tokenizer::TokKind::Compound {
                continue;
            }
            if let Some(stem) = crate::stem::light_stem(&tok.text)
                && !out.contains(&stem)
            {
                out.push(stem);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    fn record(release: &[(&str, &str)]) -> Record {
        let mut r = Record::decode(b"e1\ntype\tother").expect("a bare record");
        r.release = release
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        r
    }

    /// One term per title identity, as B13 compares them: spelling
    /// variants and alternative titles fold together; a missing year, a
    /// season pack and absolute numbering each stay apart; no title, or
    /// one without words, has none. The value is fixed across builds.
    #[test]
    fn title_identity_follows_b13s_rule() {
        let id = |r: &[(&str, &str)]| title_identity(&record(r));
        let base = id(&[("title", "Spider-Man"), ("year", "2002")]);
        assert!(base.is_some());
        for same in [
            &[("title", "Spider Man"), ("year", "2002")][..],
            &[("title", "SpiderMan"), ("year", "2002"), ("year", "2002")],
            &[("title", "spider-man"), ("year", "2002"), ("alt", "Other")],
        ] {
            assert_eq!(id(same), base, "{same:?}");
        }
        for other in [
            &[("title", "Spider-Man")][..],
            &[("title", "Spider-Man"), ("year", "2004")],
            &[("title", "Spider-Man 2"), ("year", "2002")],
            &[("title", "Spider-Man"), ("year", "2002"), ("season", "1")],
        ] {
            assert_ne!(id(other), base, "{other:?}");
        }
        assert_ne!(
            id(&[("title", "Show"), ("episode", "2")]),
            id(&[("title", "Show"), ("absolute", "2")])
        );
        assert_ne!(
            id(&[("title", "Show"), ("season", "1")]),
            id(&[("title", "Show"), ("season", "1"), ("episode", "1")])
        );
        assert_eq!(
            id(&[("title", "Show"), ("season", "1-2"), ("season", "1")]),
            id(&[("title", "Show"), ("season", "1"), ("season", "1-2")])
        );
        assert_eq!(id(&[("year", "2002")]), None);
        assert_eq!(id(&[("title", "--")]), None);
        // FNV-1a of "show\x1f\x1f\x1f\x1f": a build can't change it.
        assert_eq!(id(&[("title", "Show")]), Some(0xf42e_4ea2_e59f_9558));
        // With spans, each set sorted and joined by `,` in its own slot
        // (B13b: Siblings narrow by this term, so a changed encoding
        // must bump the field's generation, or versions go unfound).
        // FNV-1a of "spider man\x1f2002\x1f\x1f\x1f".
        assert_eq!(
            id(&[("title", "Spider-Man"), ("year", "2002")]),
            Some(0xb200_e236_36b4_b7e0)
        );
        // "show\x1f\x1f1\x1f1-3,5\x1f": a season and a multi-value
        // episode set, given out of order.
        assert_eq!(
            id(&[
                ("title", "Show"),
                ("season", "1"),
                ("episode", "5"),
                ("episode", "1-3")
            ]),
            Some(0x1ebd_7def_f83f_e767)
        );
        // "show\x1f\x1f\x1f\x1f12": absolute numbering in the last slot.
        assert_eq!(
            id(&[("title", "Show"), ("absolute", "12")]),
            Some(0x1e2d_98f2_72dc_ea3b)
        );
    }

    use super::*;

    #[test]
    fn extensions() {
        assert_eq!(extension("Movie.2019.MKV").as_deref(), Some("mkv"));
        assert_eq!(extension("archive.part01.rar").as_deref(), Some("rar"));
        assert_eq!(extension(".hidden"), None);
        assert_eq!(extension("noext"), None);
        assert_eq!(extension("weird.ex t"), None);
        assert_eq!(extension("x.abcdefghijk"), None);
    }

    #[test]
    fn path_offsets_slice_the_joined_text() {
        let p = paths_tokens(&["Season 1".into(), "E01.WEB-DL.mkv".into()]);
        assert_eq!(p.text, "Season 1\nE01.WEB-DL.mkv");
        for t in &p.tokens {
            let slice = &p.text[t.offset_from..t.offset_to];
            assert!(
                tokenizer::tokenize(slice).iter().any(|x| x.text == t.text),
                "{t:?} from {slice:?}"
            );
        }
        assert!(p.tokens.iter().any(|t| t.text == "webdl"));
    }

    #[test]
    fn flags_map_to_names() {
        let names: Vec<_> = flag_names(flags::PRIVATE | flags::BACKFILL).collect();
        assert_eq!(names, ["private", "backfill"]);
    }
}
