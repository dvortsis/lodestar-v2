//! The torrent index schema (architecture §8). The trigram field arrives
//! with regex search, the swarm fields with M4.
//!
//! v5 (B6, facets): `release_year`, the year histogram's fast column.
//!
//! Only `id` and `name` are stored; everything shown comes from SQLite.

use lodestar_classify::Category;
use tantivy::schema::{
    BytesOptions, FAST, Field, INDEXED, IndexRecordOption, NumericOptions, STORED, Schema,
    TextFieldIndexing, TextOptions,
};

use crate::tokenizer::NAME_TOKENIZER;

/// Bump on any schema change: a new version is a new index directory,
/// built in the background while the old one serves (§8).
pub const SCHEMA_VERSION: u32 = 5;
/// Bump on any tokenizer change, for the same reason.
pub const TOKENIZER_VERSION: u32 = 1;

/// Field boosts for phase-1 ranking (§8).
pub const NAME_BOOST: f32 = 3.0;
pub const PATH_BOOST: f32 = 1.0;
/// Added for a word in the effective title, on top of the name boost: a
/// torrent whose files say it's the thing outranks one that only
/// mentions it in a path (§8).
pub const TITLE_BOOST: f32 = 2.0;
/// The phase-1 score of a word matched only by its stem in the title
/// (§8's "plurals and suffixes" tier): a constant, well under a typical
/// exact match's BM25, so exact matches rank first.
pub const STEM_SCORE: f32 = 0.25;

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
    /// The effective title and each alternative title (§6), with
    /// positions for `title:"…"`.
    pub title: Field,
    /// Light stems ([`crate::stem`]) of the title words, each once,
    /// words only. A query word matches here as an alternative to its
    /// exact form.
    pub title_stem: Field,
    /// Keyword facets as `key:value` (`res:2160p`, `type:movie`,
    /// `has:nfo`); see [`TagKey`](crate::query::TagKey).
    pub tag: Field,
    /// Every year, season and episode the release names (a pack or a
    /// range gives several), in the inverted index.
    pub year: Field,
    pub season: Field,
    pub episode: Field,
    /// The release's first (lowest) year, one value per torrent in a
    /// fast column, for the year histogram (B6). `year` holds every
    /// year of a span and has no column.
    pub release_year: Field,
    pub junk: Field,
    /// Per-category share of the files and of the bytes, 0–100, in
    /// [`Category::ALL`] order. Only non-zero shares are written.
    pub cat_count: [Field; 10],
    pub cat_bytes: [Field; 10],
}

/// The fast-field name of a category share.
pub fn share_field(bytes: bool, cat: Category) -> String {
    format!(
        "cat_{}_{}",
        if bytes { "bytes" } else { "count" },
        cat.as_str()
    )
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
    let multi = || NumericOptions::default().set_indexed();
    let mut shares =
        |bytes: bool| Category::ALL.map(|c| b.add_u64_field(&share_field(bytes, c), num()));
    let cat_count = shares(false);
    let cat_bytes = shares(true);
    let fields = Fields {
        id: b.add_u64_field("id", FAST | INDEXED | STORED),
        hash: b.add_bytes_field("hash", BytesOptions::default().set_indexed()),
        name: b.add_text_field("name", text(true, true)),
        path: b.add_text_field("path", text(false, false)),
        text: b.add_text_field("text", text(false, false)),
        ext: b.add_text_field("ext", keyword.clone()),
        flag: b.add_text_field("flag", keyword.clone()),
        size: b.add_u64_field("size", num()),
        files: b.add_u64_field("files", num()),
        largest: b.add_u64_field("largest", num()),
        piece_len: b.add_u64_field("piece_len", num()),
        discovered: b.add_i64_field("discovered", num()),
        metadata: b.add_i64_field("metadata", num()),
        title: b.add_text_field("title", text(false, true)),
        title_stem: b.add_text_field(
            "title_stem",
            TextOptions::default().set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("whitespace")
                    .set_index_option(IndexRecordOption::Basic)
                    .set_fieldnorms(false),
            ),
        ),
        tag: b.add_text_field("tag", keyword.clone()),
        year: b.add_u64_field("year", multi()),
        season: b.add_u64_field("season", multi()),
        episode: b.add_u64_field("episode", multi()),
        release_year: b.add_u64_field("release_year", num()),
        junk: b.add_u64_field("junk", num()),
        cat_count,
        cat_bytes,
    };
    (b.build(), fields)
}
