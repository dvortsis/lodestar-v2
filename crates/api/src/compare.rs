//! Torrents set beside each other: `/torrents/{hash}/siblings` ("Related",
//! §10.5) and `POST /compare` (§10.6). Display data comes from SQLite;
//! the index only finds sibling candidates.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use lodestar_classify::Record;
use lodestar_metainfo::{FileAttrs, InfoHash, Text};
use lodestar_search::query::{NumField, TextField};
use lodestar_search::tokenizer::{TokKind, tokenize};
use lodestar_search::{Node, Sort};
use lodestar_store::{Enriched, FileTree, Store, TorrentRow};
use utoipa::OpenApi;

use crate::discover::{shown, withheld};
use crate::dto::compare::{
    CompareRequest, CompareResponse, CompareTorrent, DiffFile, FileDiff, FileList,
    SiblingsResponse, TitleIdentity,
};
use crate::dto::discover::{HitGroup, Unavailable};
use crate::dto::search::SearchHit;
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{self, Path, same_origin_json};
use crate::{ApiState, auth, search, torrents, unix_now};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(siblings, compare))]
pub(crate) struct ApiDoc;

/// Other versions listed, at most.
pub const MAX_SIBLINGS: usize = 20;
/// Index candidates read for other versions, best first: the title
/// phrase also matches longer titles, which the identity check drops.
const SIBLING_CANDIDATES: usize = 1000;
/// Torrents in one comparison.
pub const COMPARE_MIN: usize = 2;
pub const COMPARE_MAX: usize = 5;
/// Files listed per file-diff list, at most.
pub const DIFF_FILES: usize = 2000;

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/torrents/{hash}/siblings", get(siblings))
            .route("/compare", post(compare)),
    )
}

fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "torrent_not_found",
        "There's no torrent with that hash.",
    )
}

/// A torrent by hash with its enrichment, as long as it may be shown:
/// one that's unknown, or withheld from view ([`withheld`]), is the same
/// 404, so its name never leaks.
fn visible(
    store: &Store,
    hash: &InfoHash,
    hidden_ok: bool,
) -> ApiResult<(TorrentRow, Option<Enriched>)> {
    let id = store.find(hash).map_err(internal)?.ok_or_else(not_found)?;
    let row = store.torrent(id).map_err(internal)?.ok_or_else(not_found)?;
    let enrichment = store.enrichment(id).map_err(internal)?;
    if withheld(&row, enrichment.as_ref(), hidden_ok) {
        return Err(not_found());
    }
    Ok((row, enrichment))
}

fn record(e: Option<&Enriched>) -> Option<Record> {
    e.and_then(|e| Record::decode(&e.data).ok())
}

/// A search result row for `row`, its classification fields filled as
/// search fills them.
fn hit_row(row: &TorrentRow, score: f32, e: Option<Enriched>) -> SearchHit {
    let mut hit = torrents::hit(row, score, None);
    search::enrich(&mut hit, e);
    hit
}

// ---- siblings --------------------------------------------------------------

/// The title's words as search's tokenizer folds them: its parts, so
/// `Spider-Man`, `Spider Man` and `SpiderMan` are all `spider man`.
fn title_words(title: &str) -> Vec<String> {
    tokenize(title)
        .into_iter()
        .filter(|t| t.kind == TokKind::Part)
        .map(|t| t.text)
        .collect()
}

fn spans(r: &Record, key: &str) -> Vec<String> {
    let mut v: Vec<String> = r.release_all(key).map(str::to_string).collect();
    v.sort();
    v.dedup();
    v
}

/// A torrent's title identity (§16, B13): `None` without a parsed title
/// that has any words.
fn identity(r: &Record) -> Option<TitleIdentity> {
    let words = title_words(r.release("title")?);
    if words.is_empty() {
        return None;
    }
    Some(TitleIdentity {
        absolute: spans(r, "absolute"),
        episodes: spans(r, "episode"),
        seasons: spans(r, "season"),
        title_words: words,
        years: spans(r, "year"),
    })
}

/// A span's bounds (`2008`, `1-10`).
fn bounds(span: &str) -> Option<(u64, u64)> {
    let (a, b) = span.split_once('-').unwrap_or((span, span));
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// The index query that finds candidates for `id`: the title's words as
/// a phrase in the title field, and every year, season and episode span
/// as a range. It over-matches (a longer title, a wider span), never
/// under-matches; [`identity`] decides.
fn candidates_query(id: &TitleIdentity) -> Node {
    let mut of = vec![Node::Phrase {
        field: TextField::Title,
        text: id.title_words.join(" "),
    }];
    let ranges = [
        (NumField::Year, &id.years),
        (NumField::Season, &id.seasons),
        (NumField::Episode, &id.episodes),
        // The index keeps absolute numbers with the episodes.
        (NumField::Episode, &id.absolute),
    ];
    for (field, spans) in ranges {
        for s in spans {
            if let Some((lo, hi)) = bounds(s) {
                of.push(Node::Range {
                    field,
                    lo: Some(lo),
                    hi: Some(hi),
                });
            }
        }
    }
    Node::And { of }
}

/// Torrents related to one (§10.5): other versions of the same work,
/// identical-payload clones and the same file elsewhere. The last two
/// need data M7 adds, so they're `unavailable` until then.
#[utoipa::path(
    operation_id = "getTorrentSiblings",
    get,
    path = "/torrents/{hash}/siblings",
    tag = "torrents",
    params(("hash" = String, Path, description = "A 40-character hex info hash.")),
    responses((status = 200, body = SiblingsResponse))
)]
pub(crate) async fn siblings(
    State(s): State<ApiState>,
    Path(hash): Path<String>,
) -> ApiResult<Json<SiblingsResponse>> {
    let hash = torrents::parse_hash(&hash)?;
    tokio::task::spawn_blocking(move || {
        let (row, enrichment) = {
            let store = s.reader.lock().expect("reader lock");
            // The drawer opens torrents a filter hid (`hidden=true`), so
            // its Related does too; Adult while hidden is a 404.
            visible(&store, &hash, true)?
        };
        let identity = record(enrichment.as_ref()).as_ref().and_then(identity);
        let other_versions = match (&identity, &s.index) {
            (None, _) => HitGroup::unavailable(Unavailable::NoTitle),
            (Some(_), None) => HitGroup::unavailable(Unavailable::SearchDisabled),
            (Some(id), Some(index)) => other_versions(&s, index, row.id, id)?,
        };
        Ok(Json(SiblingsResponse {
            hash: torrents::hex(&row.info_hash.0),
            // TODO(M7): `payload_sig` and per-file `pieces_root` arrive
            // with schema bump #2 (§8); then these are found in the index.
            identical_payload: HitGroup::unavailable(Unavailable::NeedsPayloadSignature),
            identity,
            other_versions,
            same_file: HitGroup::unavailable(Unavailable::NeedsPieceRoots),
        }))
    })
    .await
    .map_err(internal)?
}

fn other_versions(
    s: &ApiState,
    search: &lodestar_search::SearchHandle,
    id: i64,
    identity: &TitleIdentity,
) -> ApiResult<HitGroup> {
    let index = search.index();
    let node = shown(candidates_query(identity));
    // Relevance: the title field's BM25 ranks the exact title over
    // longer ones. The swarm and freshness priors join it with M4.
    let results = index
        .search_page(
            &index.query(&node, unix_now()),
            Sort::Relevance,
            SIBLING_CANDIDATES,
            0,
        )
        .map_err(internal)?;
    let store = s.reader.lock().expect("reader lock");
    let mut hits = Vec::new();
    let mut truncated = false;
    for h in &results.hits {
        if h.id as i64 == id {
            continue;
        }
        // Deleted since indexing: it drops out.
        let Some(row) = store.torrent(h.id as i64).map_err(internal)? else {
            continue;
        };
        let e = store.enrichment(row.id).map_err(internal)?;
        // The index may lag the store: the stored state decides.
        if withheld(&row, e.as_ref(), false) {
            continue;
        }
        let Some(theirs) = record(e.as_ref()).as_ref().and_then(self::identity) else {
            continue;
        };
        if theirs != *identity {
            continue;
        }
        if hits.len() == MAX_SIBLINGS {
            truncated = true;
            break;
        }
        hits.push(hit_row(&row, h.score, e));
    }
    Ok(HitGroup::available(hits, truncated))
}

// ---- compare ---------------------------------------------------------------

/// 2–5 torrents side by side (§10.6): each one's summary and parsed
/// attributes, which attributes differ, the composition's shared scale
/// and the file lists compared by path and size.
///
/// A hash that's unknown, hidden by a filter rule, or Adult while Adult
/// is hidden is the same 404 `torrent_not_found`, which names nothing.
#[utoipa::path(
    operation_id = "compareTorrents",
    post,
    path = "/compare",
    tag = "torrents",
    request_body = CompareRequest,
    responses((status = 200, body = CompareResponse))
)]
pub(crate) async fn compare(
    State(s): State<ApiState>,
    headers: HeaderMap,
    extract::Json(body): extract::Json<CompareRequest>,
) -> ApiResult<Json<CompareResponse>> {
    same_origin_json(&headers)?;
    let n = body.hashes.len();
    if !(COMPARE_MIN..=COMPARE_MAX).contains(&n) {
        return Err(ApiError::bad_request(
            "compare_count",
            format!("Compare takes {COMPARE_MIN} to {COMPARE_MAX} torrents; this asked for {n}."),
        )
        .field("hashes"));
    }
    let hashes = body
        .hashes
        .iter()
        .map(|h| torrents::parse_hash(&h.to_ascii_lowercase()).map_err(|e| e.field("hashes")))
        .collect::<ApiResult<Vec<_>>>()?;
    if hashes.iter().collect::<BTreeSet<_>>().len() != n {
        return Err(ApiError::bad_request(
            "duplicate_hash",
            "Each torrent may be in a comparison once.",
        )
        .field("hashes"));
    }
    tokio::task::spawn_blocking(move || {
        let mut store = s.reader.lock().expect("reader lock");
        let mut rows = Vec::with_capacity(n);
        for h in &hashes {
            let (row, e) = visible(&store, h, false)?;
            rows.push((row, e));
        }
        let records: Vec<Option<Record>> = rows.iter().map(|(_, e)| record(e.as_ref())).collect();
        let keys: BTreeSet<&str> = records
            .iter()
            .flatten()
            .flat_map(|r| r.release.iter().map(|(k, _)| k.as_str()))
            .collect();
        let values: Vec<BTreeMap<String, Option<String>>> = records
            .iter()
            .map(|r| {
                keys.iter()
                    .map(|k| {
                        let v = r.as_ref().and_then(|r| {
                            let all: Vec<&str> = r.release_all(k).collect();
                            (!all.is_empty()).then(|| all.join(", "))
                        });
                        ((*k).to_string(), v)
                    })
                    .collect()
            })
            .collect();
        let differs = keys
            .iter()
            .map(|k| {
                let first = &values[0][*k];
                ((*k).to_string(), values.iter().any(|v| v[*k] != *first))
            })
            .collect();
        let file_diff = file_diff(&mut *store, rows.iter().map(|(r, _)| r.id))?;
        let scale_bytes = rows.iter().map(|(r, _)| r.size).max().unwrap_or(0);
        let torrents = rows
            .into_iter()
            .zip(values)
            .map(|((row, e), attributes)| column(&row, e, attributes))
            .collect();
        Ok(Json(CompareResponse {
            differs,
            file_diff,
            scale_bytes,
            torrents,
        }))
    })
    .await
    .map_err(internal)?
}

fn column(
    row: &TorrentRow,
    e: Option<Enriched>,
    attributes: BTreeMap<String, Option<String>>,
) -> CompareTorrent {
    let hit = hit_row(row, 0.0, e);
    CompareTorrent {
        attributes,
        composition: hit.composition,
        content_type: hit.content_type,
        discovered_at: row.discovered_at,
        files: row.file_count,
        hash: hit.hash,
        leechers: hit.leechers,
        name: row.name.clone(),
        seeders: hit.seeders,
        size: row.size,
        title: hit.title,
        trackers_checked: hit.trackers_checked,
        trackers_reporting: hit.trackers_reporting,
    }
}

/// A file's diff key: 128 bits of its decoded path and size. Two
/// independent SipHash runs; a false match is about 2⁻¹²⁸ per pair.
type Key = u128;

fn file_key(path: &str, size: u64) -> Key {
    let half = |salt: u8| {
        let mut h = std::hash::DefaultHasher::new();
        salt.hash(&mut h);
        path.hash(&mut h);
        size.hash(&mut h);
        h.finish()
    };
    (u128::from(half(0)) << 64) | u128::from(half(1))
}

fn file_path(tree: &FileTree, f: &lodestar_metainfo::File) -> String {
    let mut out = String::new();
    for (i, seg) in f.path.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(
            &Text {
                raw: seg.clone(),
                encoding: tree.encoding,
            }
            .decode(),
        );
    }
    out
}

/// Each non-padding file of a tree, with its path and key.
fn tree_files(tree: &FileTree) -> impl Iterator<Item = (Key, String, u64)> + '_ {
    tree.files
        .iter()
        .filter(|f| !f.attrs.contains(FileAttrs::PADDING))
        .map(|f| {
            let path = file_path(tree, f);
            (file_key(&path, f.size), path, f.size)
        })
}

fn tree(store: &mut Store, id: i64) -> ApiResult<FileTree> {
    store
        .files(id)
        .map_err(internal)?
        .ok_or_else(|| internal("a stored torrent without its file list"))
}

/// The file lists compared by path and size.
///
/// Memory stays bounded however large the trees (up to metainfo's
/// 200,000 files each): one decoded tree is held at a time, and between
/// passes only each torrent's sorted keys (16 bytes a file), so five
/// maximal trees cost about 16 MB. The listed entries are capped at
/// [`DIFF_FILES`] per list.
fn file_diff(store: &mut Store, ids: impl Iterator<Item = i64>) -> ApiResult<FileDiff> {
    let ids: Vec<i64> = ids.collect();
    let mut sets: Vec<Vec<Key>> = Vec::with_capacity(ids.len());
    for &id in &ids {
        let tree = tree(store, id)?;
        let mut keys: Vec<Key> = tree_files(&tree).map(|(k, _, _)| k).collect();
        keys.sort_unstable();
        keys.dedup();
        sets.push(keys);
    }
    let has = |i: usize, k: &Key| sets[i].binary_search(k).is_ok();
    let n = sets.len();
    let common_count = sets[0]
        .iter()
        .filter(|k| (1..n).all(|j| has(j, k)))
        .count() as u64;
    let only_counts: Vec<u64> = (0..n)
        .map(|i| {
            sets[i]
                .iter()
                .filter(|k| (0..n).all(|j| j == i || !has(j, k)))
                .count() as u64
        })
        .collect();
    // The listed entries, in each torrent's own order, one tree at a time.
    let mut common = FileList::new(common_count);
    let mut only: Vec<FileList> = only_counts.iter().map(|&c| FileList::new(c)).collect();
    for (i, &id) in ids.iter().enumerate() {
        let wants_common = i == 0 && common_count > 0;
        if !wants_common && only_counts[i] == 0 {
            continue;
        }
        let tree = tree(store, id)?;
        let mut seen: BTreeSet<Key> = BTreeSet::new();
        for (k, path, size) in tree_files(&tree) {
            if !seen.insert(k) {
                continue;
            }
            if wants_common && (1..n).all(|j| has(j, &k)) {
                common.push(path, size);
            } else if (0..n).all(|j| j == i || !has(j, &k)) {
                only[i].push(path, size);
            }
            if (!wants_common || common.full()) && only[i].full() {
                break;
            }
        }
    }
    Ok(FileDiff { common, only })
}

impl FileList {
    fn new(count: u64) -> FileList {
        FileList {
            count,
            files: Vec::new(),
            truncated: count as usize > DIFF_FILES,
        }
    }

    fn full(&self) -> bool {
        self.files.len() as u64 >= self.count.min(DIFF_FILES as u64)
    }

    fn push(&mut self, path: String, size: u64) {
        if self.files.len() < DIFF_FILES {
            self.files.push(DiffFile { path, size });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(release: &[(&str, &str)]) -> Record {
        let mut r = Record::decode(b"e1\ntype\tother").expect("a bare record");
        r.release = release
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        r
    }

    #[test]
    fn title_words_fold_spelling_variants_together() {
        let want = vec!["spider".to_string(), "man".to_string()];
        for t in ["Spider-Man", "Spider Man", "SpiderMan", "spider.man", "SPIDER  MAN"] {
            assert_eq!(title_words(t), want, "{t}");
        }
        assert_ne!(title_words("Spider-Man 2"), want);
        assert_eq!(title_words("Amélie"), title_words("Amelie"));
    }

    #[test]
    fn identity_needs_title_and_keeps_numbers_apart() {
        assert_eq!(identity(&record(&[("year", "2008")])), None);
        assert_eq!(identity(&record(&[("title", "...")])), None);
        let a = identity(&record(&[("title", "The Dark Knight"), ("year", "2008")]));
        let b = identity(&record(&[
            ("title", "the dark knight"),
            ("year", "2008"),
            ("res", "720p"),
        ]));
        assert!(a.is_some());
        assert_eq!(a, b, "quality isn't identity");
        let other_year = identity(&record(&[("title", "The Dark Knight"), ("year", "2012")]));
        assert_ne!(a, other_year);
        let no_year = identity(&record(&[("title", "The Dark Knight")]));
        assert_ne!(a, no_year);
        let s1e2 = identity(&record(&[
            ("title", "Show"),
            ("season", "1"),
            ("episode", "2"),
        ]));
        let s1e3 = identity(&record(&[
            ("title", "Show"),
            ("season", "1"),
            ("episode", "3"),
        ]));
        let s1 = identity(&record(&[("title", "Show"), ("season", "1")]));
        assert_ne!(s1e2, s1e3);
        assert_ne!(s1e2, s1);
        // Absolute numbering stays apart from season numbering.
        let abs2 = identity(&record(&[("title", "Show"), ("absolute", "2")]));
        let e2 = identity(&record(&[("title", "Show"), ("episode", "2")]));
        assert_ne!(abs2, e2);
    }

    #[test]
    fn the_candidate_query_ranges_every_span() {
        let id = identity(&record(&[
            ("title", "Show.Name"),
            ("season", "1-3"),
            ("absolute", "12"),
        ]))
        .unwrap();
        let q = candidates_query(&id).to_query();
        assert_eq!(q, "title:\"show name\" s:1..3 e:12");
        // It's a query the parser can read back.
        assert_eq!(lodestar_search::parse(&q).unwrap(), candidates_query(&id));
    }

    #[test]
    fn file_keys_match_on_path_and_size() {
        assert_eq!(file_key("a/b.mkv", 10), file_key("a/b.mkv", 10));
        assert_ne!(file_key("a/b.mkv", 10), file_key("a/b.mkv", 11));
        assert_ne!(file_key("a/b.mkv", 10), file_key("a/c.mkv", 10));
        // No ambiguity between the path and the size.
        assert_ne!(file_key("a1", 0), file_key("a", 10));
    }

    #[test]
    fn a_file_list_caps_its_entries() {
        let mut l = FileList::new(DIFF_FILES as u64 + 5);
        assert!(l.truncated);
        for i in 0..DIFF_FILES + 5 {
            l.push(format!("{i}"), 1);
        }
        assert_eq!(l.files.len(), DIFF_FILES);
        assert!(l.full());
        let mut l = FileList::new(2);
        assert!(!l.truncated && !l.full());
        l.push("a".into(), 1);
        l.push("b".into(), 1);
        assert!(l.full());
    }
}
