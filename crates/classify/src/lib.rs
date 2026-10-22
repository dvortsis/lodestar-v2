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
pub use junk::{Junk, JunkReason};
pub use record::{ENRICHMENT_VERSION, Record, RecordError};
pub use structure::Structure;

/// Bumped whenever a change alters any classification, so stored
/// results can be re-derived (§6 "Re-derivation without the raw dict").
pub const CLASSIFY_VERSION: u32 = 2;

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

pub fn classify(name: &str, files: &[FileInfo]) -> Classification {
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
    let adult_kind = adult::detect(
        name,
        &title.release,
        largest.map(|i| &files[i]),
        content_type,
    )
    .map(|(kind, score)| {
        content_type = ContentType::Adult;
        confidence = score;
        kind
    });
    let junk = junk::score(&title, &composition, structure, files);
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
