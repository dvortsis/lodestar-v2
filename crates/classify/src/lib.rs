//! Classification (architecture §6 "Derived" and "Classification", §14
//! M3): what a torrent is made of and what it is.
//!
//! [`classify`] takes the torrent name and its file list and returns:
//! - the [`Composition`]: per-category counts and bytes, from the
//!   extension registry ([`ext`]);
//! - [`Structure`] fingerprints: disc layouts, rips, scene sets, repacks;
//! - the [`Effective`] title: the release parse of the name, or, for an
//!   uninformative name, of the dominant files or top folder;
//! - the [`ContentType`] with a confidence, and for adult content its
//!   [`AdultKind`] (§20.9);
//! - the [`Junk`] score with its reasons.
//!
//! It's pure and CPU-only, so it runs on the worker side of the prepare
//! step (§19.4). The cost is one pass over the files plus at most a few
//! release parses, whatever the file count.

mod adult;
mod composition;
mod content;
mod effective;
pub mod ext;
mod junk;
mod record;
mod structure;

use std::collections::HashSet;

pub use adult::{ADULT_THRESHOLD, AdultKind};
pub use composition::{COMP_VERSION, CategoryStat, Composition, DecodeError};
pub use content::ContentType;
pub use effective::{Effective, ResolvedVia};
pub use ext::Category;
pub use junk::{Junk, JunkReason, lure_hits};
pub use record::{ENRICHMENT_VERSION, Record, RecordError};
pub use structure::Structure;

/// Bumped whenever a change alters any classification, so stored
/// results can be re-derived (§6 "Re-derivation without the raw dict").
pub const CLASSIFY_VERSION: u32 = 3;

/// One file of a torrent, with its path decoded to text. A single-file
/// torrent is one entry whose path is the torrent name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    /// Path segments below the torrent's root. Never empty.
    pub path: Vec<String>,
    pub size: u64,
    /// A BEP 47 padding file: not content.
    pub padding: bool,
}

impl FileInfo {
    pub fn new(path: Vec<String>, size: u64) -> FileInfo {
        FileInfo {
            path,
            size,
            padding: false,
        }
    }

    /// The file name: the last path segment (empty for an empty path).
    pub fn name(&self) -> &str {
        self.path.last().map_or("", String::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub composition: Composition,
    pub structure: Structure,
    pub title: Effective,
    pub content_type: ContentType,
    /// 0–100: how sure the content type is.
    pub confidence: u8,
    /// Set exactly when the content type is [`ContentType::Adult`].
    pub adult_kind: Option<AdultKind>,
    pub junk: Junk,
    /// Index of the largest non-padding file.
    pub largest: Option<usize>,
    /// Folder levels below the root (0 for a flat torrent).
    pub depth: u16,
    pub distinct_extensions: u16,
}

impl Classification {
    /// This classification relabelled adult, as an Is adult correction
    /// makes it ([`classify_corrected`]): detection's adult kind if it found
    /// one, else the nearest kind for the content type.
    pub fn as_adult(&self) -> Classification {
        let mut c = self.clone();
        if c.adult_kind.is_none() {
            c.adult_kind = Some(adult::forced_kind(c.content_type));
            c.confidence = 100;
        }
        c.content_type = ContentType::Adult;
        c
    }
}

/// A correction you made to one torrent's classification (§20.7, §20.9).
/// It's kept with the torrent and applied every time it's classified
/// again, so a newer parser doesn't undo it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Correction {
    /// `Some(true)`: Is adult. `Some(false)`: Not adult.
    pub adult: Option<bool>,
    /// Not junk: no junk score and no reasons.
    pub not_junk: bool,
}

/// The file list of a parsed info dict, paths decoded with `encoding`.
pub fn file_infos(
    files: &[lodestar_metainfo::File],
    encoding: lodestar_metainfo::TextEncoding,
) -> Vec<FileInfo> {
    files
        .iter()
        .map(|f| FileInfo {
            path: f
                .path
                .iter()
                .map(|seg| {
                    lodestar_metainfo::Text {
                        raw: seg.clone(),
                        encoding,
                    }
                    .decode()
                    .into_owned()
                })
                .collect(),
            size: f.size,
            padding: f.is_padding(),
        })
        .collect()
}

pub fn classify(name: &str, files: &[FileInfo]) -> Classification {
    classify_corrected(name, files, Correction::default())
}

/// [`classify`], with your correction applied.
pub fn classify_corrected(name: &str, files: &[FileInfo], fix: Correction) -> Classification {
    let mut composition = Composition::default();
    let mut largest: Option<usize> = None;
    let mut depth = 0usize;
    let mut extensions: HashSet<ext::Ext> = HashSet::new();
    for (i, f) in files.iter().enumerate() {
        if f.padding {
            composition.add_padding(f.size);
            continue;
        }
        let e = ext::extension(f.name());
        composition.add(
            e.as_ref()
                .map_or(Category::Other, |e| ext::category_of(e.as_str())),
            f.size,
        );
        if let Some(e) = e {
            extensions.insert(e);
        }
        if largest.is_none_or(|l| f.size > files[l].size) {
            largest = Some(i);
        }
        depth = depth.max(f.path.len().saturating_sub(1));
    }
    let structure = Structure::scan(files);
    let title = effective::resolve(name, files, &composition);
    let (mut content_type, mut confidence) =
        content::content_type(&title, &composition, structure, files);
    let detected = match fix.adult {
        Some(false) => None,
        _ => adult::detect(
            name,
            &title.release,
            largest.map(|i| &files[i]),
            content_type,
        ),
    };
    let adult_kind = match (detected, fix.adult) {
        (Some(d), _) => Some(d),
        (None, Some(true)) => Some((adult::forced_kind(content_type), 100)),
        (None, _) => None,
    }
    .map(|(kind, score)| {
        content_type = ContentType::Adult;
        confidence = score;
        kind
    });
    let junk = if fix.not_junk {
        Junk::default()
    } else {
        junk::score(&title, &composition, structure, files)
    };
    Classification {
        composition,
        structure,
        title,
        content_type,
        confidence,
        adult_kind,
        junk,
        largest,
        depth: u16::try_from(depth).unwrap_or(u16::MAX),
        distinct_extensions: u16::try_from(extensions.len()).unwrap_or(u16::MAX),
    }
}
