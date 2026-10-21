//! A torrent as the index sees it, built from the store's row and file
//! tree.

use lodestar_metainfo::{InfoHash, Text};
use lodestar_store::{FileTree, TorrentRow, flags};
use tantivy::TantivyDocument;
use tantivy::tokenizer::PreTokenizedString;

use crate::schema::Fields;
use crate::tokenizer::{self, MAX_NAME_TOKENS};

/// Path tokens per torrent. Huge packs keep their first files' paths; the
/// file index (M3) covers the rest.
pub const MAX_PATH_TOKENS: usize = 20_000;
const MAX_EXT_BYTES: usize = 10;

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
}

impl TorrentDoc {
    pub fn new(row: &TorrentRow, tree: &FileTree) -> TorrentDoc {
        let mut dirs: Vec<String> = Vec::new();
        let mut seen_dirs = std::collections::HashSet::new();
        let mut names = Vec::new();
        let mut exts: Vec<String> = Vec::new();
        let mut largest = 0;
        for f in tree.files.iter().filter(|f| !f.is_padding()) {
            largest = largest.max(f.size);
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
            if let Some(ext) = extension(name) {
                if !exts.contains(&ext) {
                    exts.push(ext);
                }
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
        d.add_u64(f.size, self.size);
        d.add_u64(f.files, self.files);
        d.add_u64(f.largest, self.largest);
        d.add_u64(f.piece_len, self.piece_len);
        d.add_i64(f.discovered, self.discovered_at);
        d.add_i64(f.metadata, self.metadata_at);
        d
    }
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
    ];
    NAMES
        .iter()
        .filter(move |(bit, _)| bits & bit != 0)
        .map(|(_, n)| *n)
}

#[cfg(test)]
mod tests {
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
