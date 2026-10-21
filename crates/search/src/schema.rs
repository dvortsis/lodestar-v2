//! The torrent index schema (architecture §8), as far as M2 fills it.
//! Parsed-title, category and composition fields arrive with enrichment
//! (M3), the trigram field with regex search.
//!
//! Only `id` and `name` are stored; everything shown comes from SQLite.

use tantivy::schema::{
    BytesOptions, FAST, Field, INDEXED, IndexRecordOption, NumericOptions, STORED, Schema,
    TextFieldIndexing, TextOptions,
};

use crate::tokenizer::NAME_TOKENIZER;

/// Bump on any schema change: a new version is a new index directory,
/// built in the background while the old one serves (§8).
pub const SCHEMA_VERSION: u32 = 2;
/// Bump on any tokenizer change, for the same reason.
pub const TOKENIZER_VERSION: u32 = 1;

/// Field boosts for phase-1 ranking (§8).
pub const NAME_BOOST: f32 = 3.0;
pub const PATH_BOOST: f32 = 1.0;

#[derive(Debug, Clone, Copy)]
pub struct Fields {
    pub id: Field,
    /// Every 20-byte swarm hash (v1, truncated v2).
    pub hash: Field,
    pub name: Field,
    /// Each distinct directory name once, then every file name. Words
    /// only, no positions: phrase and NEAR within one file name are the
    /// file index's job (M3), and positions here would cost more than the
    /// whole budget allows (§3, §16).
    pub path: Field,
    /// The name and the path entries together, words only. An unfielded
    /// word matches here, one posting list instead of a name-or-path
    /// union per word, which halves the cost of broad multi-word queries
    /// (§16); `name` adds its boost on top.
    pub text: Field,
    /// Lowercased file extensions (keyword).
    pub ext: Field,
    /// Flag names (keyword): private, v2, hybrid, …
    pub flag: Field,
    pub size: Field,
    pub files: Field,
    pub largest: Field,
    pub piece_len: Field,
    pub discovered: Field,
    pub metadata: Field,
}

pub fn build() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let text = |stored: bool, positions: bool| {
        let opts = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(NAME_TOKENIZER)
                .set_index_option(if positions {
                    IndexRecordOption::WithFreqsAndPositions
                } else {
                    IndexRecordOption::WithFreqs
                }),
        );
        if stored { opts.set_stored() } else { opts }
    };
    let keyword = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("raw")
                .set_index_option(IndexRecordOption::Basic)
                .set_fieldnorms(false),
        )
        .set_fast("raw");
    // Range filters run on fast fields; an inverted index would only add
    // bytes (§3).
    let num = || NumericOptions::default().set_fast();
    let fields = Fields {
        id: b.add_u64_field("id", FAST | INDEXED | STORED),
        hash: b.add_bytes_field("hash", BytesOptions::default().set_indexed()),
        name: b.add_text_field("name", text(true, true)),
        path: b.add_text_field("path", text(false, false)),
        text: b.add_text_field("text", text(false, false)),
        ext: b.add_text_field("ext", keyword.clone()),
        flag: b.add_text_field("flag", keyword),
        size: b.add_u64_field("size", num()),
        files: b.add_u64_field("files", num()),
        largest: b.add_u64_field("largest", num()),
        piece_len: b.add_u64_field("piece_len", num()),
        discovered: b.add_i64_field("discovered", num()),
        metadata: b.add_i64_field("metadata", num()),
    };
    (b.build(), fields)
}
