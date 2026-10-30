use std::collections::HashMap;

use lodestar_bencode::{self as bencode, DecodeOptions, Dict, Encoder, Value};
use sha1::{Digest, Sha1};
use sha2::Sha256;

use crate::error::{Error, Warning};
use crate::hash::{InfoHash, InfoHashV2, decode_hex};
use crate::text::{Text, TextEncoding};

/// Caps on hostile input. Defaults follow architecture §6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Info dictionaries larger than this are rejected (recorded upstream as
    /// "oversize" with hash and size only).
    pub max_info_bytes: usize,
    /// Cap for a whole `.torrent` file, which can carry v2 piece layers.
    pub max_torrent_bytes: usize,
    /// Bencode value budget; bounds decoder memory.
    pub max_items: usize,
    pub max_files: usize,
    /// Segments per path.
    pub max_path_depth: usize,
    /// Bytes per path, summed over segments.
    pub max_path_bytes: usize,
    pub max_name_bytes: usize,
    /// Unknown info keys are kept as a bencode blob up to this size.
    pub max_extra_bytes: usize,
}

impl Limits {
    pub const DEFAULT: Limits = Limits {
        max_info_bytes: 16 << 20,
        max_torrent_bytes: 32 << 20,
        max_items: 2_000_000,
        max_files: 200_000,
        max_path_depth: 64,
        max_path_bytes: 4096,
        max_name_bytes: 4096,
        max_extra_bytes: 64 << 10,
    };

    pub(crate) fn bencode(&self) -> bencode::Limits {
        // A v2 file tree nests one dictionary per path segment plus a few
        // structural levels (info → file tree → … → "" → properties).
        bencode::Limits {
            max_depth: self.max_path_depth.saturating_add(8),
            max_items: self.max_items,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Limits::DEFAULT
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ParseOptions {
    pub limits: Limits,
    /// Encoding for non-UTF-8 names when the source says which one it is
    /// (a `.torrent`'s top-level `encoding` key). Detection is used otherwise.
    pub encoding_hint: Option<TextEncoding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetaVersion {
    V1,
    V2,
    /// Carries both a v1 file list and a v2 file tree (BEP 52).
    Hybrid,
}

/// BEP 47 file attributes, plus legacy padding-file detection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct FileAttrs(u8);

impl FileAttrs {
    pub const PADDING: FileAttrs = FileAttrs(1);
    pub const HIDDEN: FileAttrs = FileAttrs(2);
    pub const EXECUTABLE: FileAttrs = FileAttrs(4);
    pub const SYMLINK: FileAttrs = FileAttrs(8);
    const ALL: u8 = 0x0f;

    pub const fn empty() -> FileAttrs {
        FileAttrs(0)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn from_bits_truncate(bits: u8) -> FileAttrs {
        FileAttrs(bits & Self::ALL)
    }

    pub const fn contains(self, other: FileAttrs) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: FileAttrs) {
        self.0 |= other.0;
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn parse(attr: &[u8]) -> FileAttrs {
        let mut out = FileAttrs::empty();
        for c in attr {
            match c {
                b'p' => out.insert(FileAttrs::PADDING),
                b'h' => out.insert(FileAttrs::HIDDEN),
                b'x' => out.insert(FileAttrs::EXECUTABLE),
                b'l' => out.insert(FileAttrs::SYMLINK),
                _ => {}
            }
        }
        out
    }
}

/// Optional per-file content hashes some creators embed in v1 torrents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct FileHashes {
    pub md5: Option<[u8; 16]>,
    pub sha1: Option<[u8; 20]>,
    pub ed2k: Option<[u8; 16]>,
}

impl FileHashes {
    pub fn is_empty(&self) -> bool {
        self.md5.is_none() && self.sha1.is_none() && self.ed2k.is_none()
    }
}

/// One file. Paths are relative to the torrent root (the `name` directory
/// for multi-file torrents); a single-file torrent has one file whose path is
/// `[name]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct File {
    /// Raw path segments, in [`Info::path_encoding`]. Never empty.
    pub path: Vec<Vec<u8>>,
    pub size: u64,
    pub attrs: FileAttrs,
    /// Link target segments (BEP 47), present only with [`FileAttrs::SYMLINK`].
    pub symlink_target: Option<Vec<Vec<u8>>>,
    /// v2 Merkle root: exact file identity across torrents.
    pub pieces_root: Option<[u8; 32]>,
    pub hashes: FileHashes,
}

impl File {
    pub fn is_padding(&self) -> bool {
        self.attrs.contains(FileAttrs::PADDING)
    }
}

/// A parsed info dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// SHA-1 of the info dict; absent for pure v2 torrents.
    pub v1: Option<InfoHash>,
    /// SHA-256 of the info dict; present for v2 and hybrid torrents.
    pub v2: Option<InfoHashV2>,
    pub version: MetaVersion,
    pub name: Text,
    pub piece_length: u64,
    pub piece_count: u64,
    /// Sum of non-padding file sizes.
    pub total_size: u64,
    pub private: bool,
    /// The `source` key (private-tracker tagging), raw.
    pub source: Option<Vec<u8>>,
    /// v1 single-file layout (`length` rather than `files`).
    pub single_file: bool,
    /// Files in torrent order (v1 order; sorted path order for pure v2).
    pub files: Vec<File>,
    pub path_encoding: TextEncoding,
    /// Unknown info keys as a canonical bencode dictionary.
    pub extra: Option<Vec<u8>>,
    /// Size of the raw info dictionary in bytes.
    pub info_size: usize,
    pub warnings: Vec<Warning>,
}

const KNOWN_KEYS: &[&[u8]] = &[
    b"attr",
    b"ed2k",
    b"file tree",
    b"files",
    b"length",
    b"md5sum",
    b"meta version",
    b"name",
    b"name.utf-8",
    b"piece length",
    b"pieces",
    b"private",
    b"sha1",
    b"source",
    b"symlink path",
];

/// The path given to a padding file that has none (BEP 47 allows that).
/// Files need a non-empty path; `.pad/` is the usual padding directory, so
/// the file also reads as padding by name.
const PADDING_PATH: &[&[u8]] = &[b".pad", b"padding"];

/// A file before the torrent-wide encoding decision is made.
struct Draft<'a> {
    path: Vec<&'a [u8]>,
    path_utf8: Option<Vec<&'a [u8]>>,
    size: u64,
    attrs: FileAttrs,
    symlink_target: Option<Vec<&'a [u8]>>,
    pieces_root: Option<[u8; 32]>,
    hashes: FileHashes,
}

#[derive(Default)]
struct Warnings(Vec<Warning>);

impl Warnings {
    fn push(&mut self, w: Warning) {
        if !self.0.contains(&w) {
            self.0.push(w);
        }
    }
}

impl Info {
    pub fn parse(raw: &[u8]) -> Result<Info, Error> {
        Info::parse_with(raw, &ParseOptions::default())
    }

    pub fn parse_with(raw: &[u8], opts: &ParseOptions) -> Result<Info, Error> {
        let limits = &opts.limits;
        if raw.len() > limits.max_info_bytes {
            return Err(Error::TooLarge {
                size: raw.len(),
                limit: limits.max_info_bytes,
            });
        }
        let value =
            bencode::decode_with(raw, &DecodeOptions::lenient().with_limits(limits.bencode()))?;
        let dict = value.as_dict().ok_or(wrong_type("info", "dictionary"))?;
        let mut warnings = Warnings::default();

        let version = match get_int(dict, "meta version")?.unwrap_or(1) {
            1 => MetaVersion::V1,
            2 if [&b"pieces"[..], b"length", b"files"]
                .iter()
                .any(|k| dict.contains_key(k)) =>
            {
                MetaVersion::Hybrid
            }
            2 => MetaVersion::V2,
            v => return Err(Error::UnsupportedMetaVersion(v)),
        };

        let piece_length =
            get_u64(dict, "piece length")?.ok_or(Error::MissingField("piece length"))?;
        if piece_length == 0 {
            return Err(invalid("piece length", "zero"));
        }
        if version != MetaVersion::V1
            && (piece_length < 16 << 10 || !piece_length.is_power_of_two())
        {
            return Err(invalid(
                "piece length",
                "v2 requires a power of two of at least 16 KiB",
            ));
        }

        let name_raw = get_bytes(dict, "name")?.ok_or(Error::MissingField("name"))?;
        let name_utf8 = get_bytes(dict, "name.utf-8")
            .ok()
            .flatten()
            .filter(|n| std::str::from_utf8(n).is_ok());
        if name_utf8.unwrap_or(name_raw).len() > limits.max_name_bytes {
            return Err(Error::LimitExceeded {
                what: "name length",
                limit: limits.max_name_bytes,
            });
        }

        let (mut drafts, single_file) = match version {
            MetaVersion::V2 => (parse_file_tree(dict, limits, &mut warnings)?, false),
            MetaVersion::V1 | MetaVersion::Hybrid => {
                parse_v1_files(dict, name_raw, name_utf8, limits, &mut warnings)?
            }
        };

        for d in &mut drafts {
            let path = d.path_utf8.as_ref().unwrap_or(&d.path);
            let legacy_padding = path.first() == Some(&&b".pad"[..])
                || path
                    .last()
                    .is_some_and(|s| s.starts_with(b"_____padding_file_"));
            if legacy_padding {
                d.attrs.insert(FileAttrs::PADDING);
            }
            if path.iter().any(|s| suspicious_segment(s)) {
                warnings.push(Warning::SuspiciousPath);
            }
        }

        if version == MetaVersion::Hybrid {
            let tree = parse_file_tree(dict, limits, &mut warnings)?;
            attach_pieces_roots(&mut drafts, &tree, &mut warnings);
        }

        let mut all_bytes: u64 = 0;
        let mut total_size: u64 = 0;
        for d in &drafts {
            // Capped at i64::MAX so sizes fit SQLite integers downstream.
            all_bytes = all_bytes
                .checked_add(d.size)
                .filter(|&n| i64::try_from(n).is_ok())
                .ok_or(invalid("length", "total size overflows"))?;
            if !d.attrs.contains(FileAttrs::PADDING) {
                total_size += d.size;
            }
        }

        let piece_count = match version {
            MetaVersion::V2 => drafts.iter().map(|d| d.size.div_ceil(piece_length)).sum(),
            MetaVersion::V1 | MetaVersion::Hybrid => {
                let pieces = get_bytes(dict, "pieces")?.ok_or(Error::MissingField("pieces"))?;
                if pieces.len() % 20 != 0 {
                    return Err(invalid("pieces", "length is not a multiple of 20"));
                }
                let actual = (pieces.len() / 20) as u64;
                let expected = all_bytes.div_ceil(piece_length);
                if actual != expected {
                    warnings.push(Warning::PieceCountMismatch { expected, actual });
                }
                actual
            }
        };

        // One encoding decision per torrent (see `TextEncoding::detect`).
        // Each file prefers its own valid `path.utf-8`; the name (unless
        // `name.utf-8` labels it), the legacy paths left and the symlink
        // targets share one encoding.
        let mut unlabelled: Vec<&[u8]> = Vec::new();
        if name_utf8.is_none() {
            unlabelled.push(name_raw);
        }
        for d in &drafts {
            if d.path_utf8.is_none() {
                unlabelled.extend(d.path.iter().copied());
            }
            // BEP 52 strings are UTF-8, so a pure v2 target can't sway the
            // encoding of every file name.
            if version != MetaVersion::V2 {
                unlabelled.extend(d.symlink_target.iter().flatten().copied());
            }
        }
        let detected = legacy_encoding(&unlabelled, &drafts, opts.encoding_hint);
        let name = match name_utf8 {
            Some(n) => Text {
                raw: n.to_vec(),
                encoding: TextEncoding::UTF8,
            },
            None => Text {
                raw: name_raw.to_vec(),
                encoding: detected,
            },
        };
        let (path_encoding, paths) = resolve_paths(&drafts, detected);

        let files = drafts
            .into_iter()
            .zip(paths)
            .map(|(d, (path, symlink_target))| File {
                path,
                size: d.size,
                attrs: d.attrs,
                symlink_target,
                pieces_root: d.pieces_root,
                hashes: d.hashes,
            })
            .collect();

        let extra = encode_extras(dict, limits, &mut warnings);

        Ok(Info {
            v1: (version != MetaVersion::V2).then(|| InfoHash(Sha1::digest(raw).into())),
            v2: (version != MetaVersion::V1).then(|| InfoHashV2(Sha256::digest(raw).into())),
            version,
            name,
            piece_length,
            piece_count,
            total_size,
            private: get_int(dict, "private").ok().flatten() == Some(1),
            source: get_bytes(dict, "source").ok().flatten().map(<[u8]>::to_vec),
            single_file,
            files,
            path_encoding,
            extra,
            info_size: raw.len(),
            warnings: warnings.0,
        })
    }

    /// The 20-byte identity used for storage and DHT lookups: the v1 hash,
    /// or the truncated v2 hash for pure v2 torrents.
    pub fn primary_hash(&self) -> InfoHash {
        match (self.v1, self.v2) {
            (Some(v1), _) => v1,
            (None, Some(v2)) => v2.truncated(),
            (None, None) => unreachable!("an Info always has at least one hash"),
        }
    }

    /// Every 20-byte hash this torrent's swarms are announced under.
    pub fn swarm_hashes(&self) -> impl Iterator<Item = InfoHash> + '_ {
        self.v1.into_iter().chain(self.v2.map(|h| h.truncated()))
    }

    /// The file's path decoded and joined with `/`.
    pub fn path_string(&self, file: &File) -> String {
        let mut out = String::new();
        for (i, seg) in file.path.iter().enumerate() {
            if i > 0 {
                out.push('/');
            }
            out.push_str(&self.path_encoding.decode(seg));
        }
        out
    }
}

type RawPath<'a> = Vec<&'a [u8]>;

/// Files that carry both a legacy `path` and its `path.utf-8` show which
/// encoding the creator wrote; at most this many are compared.
const MAX_ENCODING_PAIRS: usize = 16;

/// The encoding of the legacy text in `unlabelled`.
///
/// Valid UTF-8 wins. Otherwise detection also reads the legacy `path` bytes
/// of files that have a `path.utf-8`: the same client wrote them, so they're
/// more evidence. Those files also give ground truth: if another encoding
/// decodes more of their legacy paths to exactly their `path.utf-8`, that
/// encoding is taken over the statistical guess (or the hint).
fn legacy_encoding(
    unlabelled: &[&[u8]],
    drafts: &[Draft<'_>],
    hint: Option<TextEncoding>,
) -> TextEncoding {
    if unlabelled.iter().all(|s| std::str::from_utf8(s).is_ok()) {
        return TextEncoding::UTF8;
    }
    let pairs: Vec<(&[&[u8]], &RawPath<'_>)> = drafts
        .iter()
        .filter_map(|d| Some((&d.path[..], d.path_utf8.as_ref()?)))
        .filter(|(raw, _)| raw.iter().any(|s| std::str::from_utf8(s).is_err()))
        .take(MAX_ENCODING_PAIRS)
        .collect();
    let evidence = unlabelled
        .iter()
        .copied()
        .chain(pairs.iter().flat_map(|(raw, _)| raw.iter().copied()));
    let detected = TextEncoding::detect(evidence, hint);
    let fits = |e: TextEncoding| {
        pairs
            .iter()
            .filter(|(raw, utf8)| {
                raw.len() == utf8.len()
                    && raw
                        .iter()
                        .zip(utf8.iter())
                        .all(|(r, u)| e.decode(r).as_bytes() == *u)
            })
            .count()
    };
    let baseline = fits(detected);
    if baseline == pairs.len() {
        return detected;
    }
    // The first best candidate, so ties keep the table's order.
    let (best, count) = TextEncoding::legacy_candidates()
        .map(|e| (e, fits(e)))
        .fold(
            (detected, baseline),
            |acc, c| if c.1 > acc.1 { c } else { acc },
        );
    if count > baseline { best } else { detected }
}

type OwnedPath = Vec<Vec<u8>>;

/// Picks each file's path and the one encoding every stored path and
/// symlink target is in.
///
/// A file with a valid `path.utf-8` uses it. Other paths, and all symlink
/// targets (BEP 47 has no UTF-8 variant), keep their legacy bytes, judged to
/// be in `detected`. When both kinds are present and the legacy bytes aren't
/// UTF-8, the UTF-8 paths are re-encoded into `detected` so the legacy raw
/// bytes survive. Only if one of them can't be written losslessly in
/// `detected` is the legacy text decoded to UTF-8 instead, because the right
/// file name matters more than the legacy bytes.
fn resolve_paths(
    drafts: &[Draft<'_>],
    detected: TextEncoding,
) -> (TextEncoding, Vec<(OwnedPath, Option<OwnedPath>)>) {
    let raw = |s: &[u8]| s.to_vec();
    let legacy = drafts
        .iter()
        .any(|d| d.path_utf8.is_none() || d.symlink_target.is_some());
    let labelled = || drafts.iter().filter_map(|d| d.path_utf8.as_ref()).flatten();
    let reencode = |s: &[u8]| {
        std::str::from_utf8(s)
            .ok()
            .and_then(|t| detected.encode_exact(t))
    };
    if !legacy || detected == TextEncoding::UTF8 {
        (TextEncoding::UTF8, map_paths(drafts, raw, raw))
    } else if labelled().next().is_none() {
        (detected, map_paths(drafts, raw, raw))
    } else if labelled().all(|s| reencode(s).is_some()) {
        let files = map_paths(drafts, |s| reencode(s).unwrap_or_else(|| s.to_vec()), raw);
        (detected, files)
    } else {
        let decode = |s: &[u8]| detected.decode(s).into_owned().into_bytes();
        (TextEncoding::UTF8, map_paths(drafts, raw, decode))
    }
}

/// Converts every path and target segment: `labelled` for `path.utf-8`
/// segments, `legacy` for everything else.
fn map_paths(
    drafts: &[Draft<'_>],
    labelled: impl Fn(&[u8]) -> Vec<u8>,
    legacy: impl Fn(&[u8]) -> Vec<u8>,
) -> Vec<(OwnedPath, Option<OwnedPath>)> {
    let legacy_path = |p: &[&[u8]]| p.iter().map(|s| legacy(s)).collect();
    drafts
        .iter()
        .map(|d| {
            let path = match &d.path_utf8 {
                Some(p) => p.iter().map(|s| labelled(s)).collect(),
                None => legacy_path(&d.path),
            };
            (path, d.symlink_target.as_deref().map(legacy_path))
        })
        .collect()
}

/// The info dictionary re-encoded without `pieces`, for the optional raw
/// archive (architecture §3: `pieces` is never stored).
pub fn info_without_pieces(raw: &[u8]) -> Result<Vec<u8>, Error> {
    let value = bencode::decode_with(
        raw,
        &DecodeOptions::lenient().with_limits(Limits::DEFAULT.bencode()),
    )?;
    let dict = value.as_dict().ok_or(wrong_type("info", "dictionary"))?;
    Ok(encode_sorted(dict.iter().filter(|(k, _)| *k != b"pieces")))
}

fn encode_sorted<'v, 'a: 'v>(entries: impl Iterator<Item = (&'a [u8], &'v Value<'a>)>) -> Vec<u8> {
    let mut entries: Vec<_> = entries.collect();
    entries.sort_unstable_by_key(|(k, _)| *k);
    let mut out = Vec::new();
    Encoder::new(&mut out).dict(|d| {
        for (k, v) in entries {
            d.entry(k).value(v);
        }
    });
    out
}

fn encode_extras(dict: &Dict<'_>, limits: &Limits, warnings: &mut Warnings) -> Option<Vec<u8>> {
    let mut unknown = dict
        .iter()
        .filter(|(k, _)| !KNOWN_KEYS.contains(k))
        .peekable();
    unknown.peek()?;
    let out = encode_sorted(unknown);
    if out.len() > limits.max_extra_bytes {
        warnings.push(Warning::ExtrasDropped { size: out.len() });
        return None;
    }
    Some(out)
}

fn parse_v1_files<'a>(
    dict: &Dict<'a>,
    name_raw: &'a [u8],
    name_utf8: Option<&'a [u8]>,
    limits: &Limits,
    warnings: &mut Warnings,
) -> Result<(Vec<Draft<'a>>, bool), Error> {
    match (dict.get(b"length"), dict.get(b"files")) {
        (Some(_), Some(_)) => Err(invalid("files", "both length and files are present")),
        (Some(_), None) => {
            let mut draft = file_props(dict, "length", limits, warnings)?;
            draft.path = vec![name_raw];
            draft.path_utf8 = name_utf8.map(|n| vec![n]);
            Ok((vec![draft], true))
        }
        (None, Some(files)) => {
            let files = files
                .as_list()
                .ok_or(wrong_type("files", "list of dictionaries"))?;
            if files.is_empty() {
                return Err(invalid("files", "empty file list"));
            }
            if files.len() > limits.max_files {
                return Err(Error::LimitExceeded {
                    what: "file count",
                    limit: limits.max_files,
                });
            }
            let mut out = Vec::with_capacity(files.len());
            for f in files.iter() {
                let f = f
                    .as_dict()
                    .ok_or(wrong_type("files", "list of dictionaries"))?;
                let mut draft = file_props(f, "length", limits, warnings)?;
                let path = f.get(b"path");
                if let Some(p) = path {
                    draft.path = path_list(p, "path", limits)?;
                }
                if draft.path.is_empty() {
                    // BEP 47 lets padding files omit their path.
                    if draft.attrs.contains(FileAttrs::PADDING) {
                        draft.path = PADDING_PATH.to_vec();
                        out.push(draft);
                        continue;
                    }
                    return Err(match path {
                        None => Error::MissingField("path"),
                        Some(_) => invalid("path", "empty path"),
                    });
                }
                // A broken `path.utf-8` just falls back to `path`.
                draft.path_utf8 = f
                    .get(b"path.utf-8")
                    .and_then(|p| path_list(p, "path.utf-8", limits).ok())
                    .filter(|p| !p.is_empty() && p.iter().all(|s| std::str::from_utf8(s).is_ok()));
                out.push(draft);
            }
            Ok((out, false))
        }
        (None, None) => Err(Error::MissingField("length or files")),
    }
}

/// Size, attributes, symlink target and hashes from a v1 file dictionary
/// (or the info dictionary itself for single-file torrents).
fn file_props<'a>(
    d: &Dict<'a>,
    length_key: &'static str,
    limits: &Limits,
    warnings: &mut Warnings,
) -> Result<Draft<'a>, Error> {
    let attrs = file_attrs(d);
    let size = file_size(d, length_key, attrs)?;
    let symlink_target = symlink_target(d, attrs, limits);
    let hashes = FileHashes {
        md5: file_hash::<16>(d, "md5sum", warnings),
        sha1: file_hash::<20>(d, "sha1", warnings),
        ed2k: file_hash::<16>(d, "ed2k", warnings),
    };
    Ok(Draft {
        path: Vec::new(),
        path_utf8: None,
        size,
        attrs,
        symlink_target,
        pieces_root: None,
        hashes,
    })
}

fn file_attrs(d: &Dict<'_>) -> FileAttrs {
    get_bytes(d, "attr")
        .ok()
        .flatten()
        .map(FileAttrs::parse)
        .unwrap_or_default()
}

/// BEP 47 lets a symlink omit its length, which is then zero.
fn file_size(d: &Dict<'_>, length_key: &'static str, attrs: FileAttrs) -> Result<u64, Error> {
    match get_u64(d, length_key)? {
        Some(size) => Ok(size),
        None if attrs.contains(FileAttrs::SYMLINK) => Ok(0),
        None => Err(Error::MissingField("length")),
    }
}

/// A malformed or over-limit target is dropped; the link itself is kept.
fn symlink_target<'a>(d: &Dict<'a>, attrs: FileAttrs, limits: &Limits) -> Option<Vec<&'a [u8]>> {
    if !attrs.contains(FileAttrs::SYMLINK) {
        return None;
    }
    d.get(b"symlink path")
        .and_then(|p| path_list(p, "symlink path", limits).ok())
}

/// Accepts the raw digest or its hex form; anything else is ignored with a warning.
fn file_hash<const N: usize>(d: &Dict<'_>, key: &str, warnings: &mut Warnings) -> Option<[u8; N]> {
    let value = d.get(key.as_bytes())?;
    let parsed = value.as_bytes().and_then(|b| {
        if b.len() == N {
            b.try_into().ok()
        } else {
            decode_hex::<N>(b)
        }
    });
    if parsed.is_none() {
        warnings.push(Warning::BadFileHash);
    }
    parsed
}

fn path_list<'a>(
    v: &Value<'a>,
    field: &'static str,
    limits: &Limits,
) -> Result<Vec<&'a [u8]>, Error> {
    let list = v
        .as_list()
        .ok_or(wrong_type(field, "list of byte strings"))?;
    if list.len() > limits.max_path_depth {
        return Err(Error::LimitExceeded {
            what: "path depth",
            limit: limits.max_path_depth,
        });
    }
    let mut out = Vec::with_capacity(list.len());
    let mut bytes = 0usize;
    for seg in list.iter() {
        let seg = seg
            .as_bytes()
            .ok_or(wrong_type(field, "list of byte strings"))?;
        bytes += seg.len();
        if bytes > limits.max_path_bytes {
            return Err(Error::LimitExceeded {
                what: "path length",
                limit: limits.max_path_bytes,
            });
        }
        out.push(seg);
    }
    Ok(out)
}

/// Flattens a BEP 52 `file tree` into files in sorted path order.
fn parse_file_tree<'a>(
    dict: &Dict<'a>,
    limits: &Limits,
    warnings: &mut Warnings,
) -> Result<Vec<Draft<'a>>, Error> {
    let tree = dict
        .get(b"file tree")
        .ok_or(Error::MissingField("file tree"))?
        .as_dict()
        .ok_or(wrong_type("file tree", "dictionary"))?;
    let mut out = Vec::new();
    let mut prefix = Vec::new();
    // Recursion depth is bounded by the bencode depth limit.
    walk_tree(tree, &mut prefix, 0, &mut out, limits, warnings)?;
    if out.is_empty() {
        return Err(invalid("file tree", "no files"));
    }
    Ok(out)
}

fn walk_tree<'a>(
    node: &Dict<'a>,
    prefix: &mut Vec<&'a [u8]>,
    prefix_bytes: usize,
    out: &mut Vec<Draft<'a>>,
    limits: &Limits,
    warnings: &mut Warnings,
) -> Result<(), Error> {
    if node.is_empty() {
        warnings.push(Warning::MalformedV2);
        return Ok(());
    }
    let mut entries: Vec<_> = node.iter().collect();
    if !node.is_sorted() {
        entries.sort_unstable_by_key(|(k, _)| *k);
    }
    for (name, child) in entries {
        if name.is_empty() {
            return Err(invalid("file tree", "empty path segment"));
        }
        let child = child
            .as_dict()
            .ok_or(wrong_type("file tree", "dictionary"))?;
        let bytes = prefix_bytes + name.len();
        if prefix.len() >= limits.max_path_depth {
            return Err(Error::LimitExceeded {
                what: "path depth",
                limit: limits.max_path_depth,
            });
        }
        if bytes > limits.max_path_bytes {
            return Err(Error::LimitExceeded {
                what: "path length",
                limit: limits.max_path_bytes,
            });
        }
        prefix.push(name);
        if let Some(props) = child.get(b"") {
            if child.len() != 1 {
                return Err(invalid(
                    "file tree",
                    "a node is both a file and a directory",
                ));
            }
            let props = props
                .as_dict()
                .ok_or(wrong_type("file tree", "dictionary"))?;
            if out.len() >= limits.max_files {
                return Err(Error::LimitExceeded {
                    what: "file count",
                    limit: limits.max_files,
                });
            }
            let attrs = file_attrs(props);
            let size = file_size(props, "length", attrs)?;
            let pieces_root = match props.get(b"pieces root").and_then(Value::as_bytes) {
                Some(r) if r.len() == 32 => r.try_into().ok(),
                _ => None,
            };
            if size > 0 && pieces_root.is_none() {
                warnings.push(Warning::MalformedV2);
            }
            out.push(Draft {
                path: prefix.clone(),
                path_utf8: None,
                size,
                attrs,
                symlink_target: symlink_target(props, attrs, limits),
                pieces_root,
                hashes: FileHashes::default(),
            });
        } else {
            walk_tree(child, prefix, bytes, out, limits, warnings)?;
        }
        prefix.pop();
    }
    Ok(())
}

/// Copies v2 `pieces root`s onto the v1 file list of a hybrid torrent and
/// checks that both views describe the same files.
fn attach_pieces_roots(v1: &mut [Draft<'_>], v2: &[Draft<'_>], warnings: &mut Warnings) {
    let by_path: HashMap<&[&[u8]], &Draft<'_>> = v2.iter().map(|d| (&d.path[..], d)).collect();
    let mut matched = 0usize;
    let mut consistent = true;
    for d in v1
        .iter_mut()
        .filter(|d| !d.attrs.contains(FileAttrs::PADDING))
    {
        let path = d.path_utf8.as_ref().unwrap_or(&d.path);
        match by_path.get(&path[..]) {
            Some(t) if t.size == d.size => {
                d.pieces_root = t.pieces_root;
                matched += 1;
            }
            _ => consistent = false,
        }
    }
    if !consistent || matched != v2.len() {
        warnings.push(Warning::HybridMismatch);
    }
}

fn suspicious_segment(s: &[u8]) -> bool {
    s.is_empty() || s == b"." || s == b".." || s.iter().any(|&b| b == b'/' || b == b'\\' || b == 0)
}

fn wrong_type(field: &'static str, expected: &'static str) -> Error {
    Error::WrongType { field, expected }
}

fn invalid(field: &'static str, reason: &'static str) -> Error {
    Error::Invalid { field, reason }
}

fn get_bytes<'a>(d: &Dict<'a>, key: &'static str) -> Result<Option<&'a [u8]>, Error> {
    match d.get(key.as_bytes()) {
        None => Ok(None),
        Some(v) => v.as_bytes().map(Some).ok_or(wrong_type(key, "byte string")),
    }
}

fn get_int(d: &Dict<'_>, key: &'static str) -> Result<Option<i64>, Error> {
    match d.get(key.as_bytes()) {
        None => Ok(None),
        Some(v) => v.as_int().map(Some).ok_or(wrong_type(key, "integer")),
    }
}

fn get_u64(d: &Dict<'_>, key: &'static str) -> Result<Option<u64>, Error> {
    match get_int(d, key)? {
        None => Ok(None),
        Some(i) => u64::try_from(i)
            .map(Some)
            .map_err(|_| invalid(key, "negative")),
    }
}
