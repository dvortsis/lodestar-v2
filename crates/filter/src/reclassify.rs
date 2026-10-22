//! Classification as it's stored (§20.9): detection, your per-torrent
//! correction and your classification rules, in the form the store keeps
//! (`Enriched`). Filtering never sees the corrected form: it judges
//! detection's own classification.

use lodestar_classify::{
    Classification, ContentType, Correction, ENRICHMENT_VERSION, FileInfo, Record,
    classify_corrected,
};
use lodestar_store::{Enriched, TorrentRow};

use crate::core::Item;
use crate::engine::Engine;
use crate::matcher::Facts;

/// The stored enrichment of a torrent with this name and file list, your
/// correction applied, and the classification it came from.
pub fn enrich_corrected(
    name: &str,
    files: &[FileInfo],
    fix: Correction,
) -> (Enriched, Classification) {
    let c = classify_corrected(name, files, fix);
    let enriched = Enriched {
        version: ENRICHMENT_VERSION,
        content_type: c.content_type.code(),
        junk_score: c.junk.score,
        comp: c.composition.encode(),
        data: Record::new(&c).encode(),
    };
    (enriched, c)
}

/// The enrichment of a stored torrent: its own correction `own`, then the
/// newest matching classification rule, then detection.
pub fn enrich_row(
    row: &TorrentRow,
    files: &[FileInfo],
    own: Correction,
    engine: Option<&Engine>,
    now: i64,
) -> (Enriched, Classification) {
    let rules = engine.filter(|e| !e.class_rules.is_empty() && own.adult.is_none());
    let Some(engine) = rules else {
        return enrich_corrected(&row.name, files, own);
    };
    let detected = enrich_corrected(&row.name, files, own);
    let class = &detected.1;
    let facts = Facts {
        class,
        size: row.size,
        piece_len: row.piece_len,
        flags: row.flags,
        discovered_at: row.discovered_at,
        now,
    };
    let item = Item {
        hashes: &[],
        name: &row.name,
        files,
        content_type: class.content_type,
    };
    match engine.class_correction(&item, Some(&facts)) {
        Some(adult) if adult != (class.content_type == ContentType::Adult) => enrich_corrected(
            &row.name,
            files,
            Correction {
                adult: Some(adult),
                ..own
            },
        ),
        _ => detected,
    }
}
