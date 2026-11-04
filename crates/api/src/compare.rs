//! Torrents set beside each other: `/torrents/{hash}/siblings` ("Related",
//! §10.5) and `POST /compare` (§10.6). Display data comes from SQLite;
//! the index only finds sibling candidates.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use lodestar_classify::Record;
use lodestar_metainfo::{FileAttrs, InfoHash, Text};
use lodestar_search::query::{NumField, TextField};
use lodestar_search::tokenizer::{TokKind, tokenize};
use lodestar_search::{CancelToken, Node, Sort};
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
use crate::swarm::{self, Summary};
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
    adult: bool,
) -> ApiResult<(TorrentRow, Option<Enriched>)> {
    let id = store.find(hash).map_err(internal)?.ok_or_else(not_found)?;
    let row = store.torrent(id).map_err(internal)?.ok_or_else(not_found)?;
    let enrichment = store.enrichment(id).map_err(internal)?;
    if withheld(&row, enrichment.as_ref(), hidden_ok, adult) {
        return Err(not_found());
    }
    Ok((row, enrichment))
}

fn record(e: Option<&Enriched>) -> Option<Record> {
    e.and_then(|e| Record::decode(&e.data).ok())
}

/// A search result row for `row`, its classification and swarm fields
/// filled as search fills them.
fn hit_row(row: &TorrentRow, score: f32, e: Option<Enriched>, swarm: Summary) -> SearchHit {
    let mut hit = torrents::hit(row, score, None, swarm);
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

/// A torrent's title identity (`docs/decisions/2026-10-22-B13.md`): `None` without a parsed title
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
    // "Show adult content", read once for the whole request.
    let adult = s.adult.shown();
    // Axum drops this future when the client disconnects (the drawer
    // closed, or moved to another torrent); the guard then cancels the
    // candidate search and its lookups on the blocking pool.
    let cancel = CancelToken::new();
    let _guard = cancel.cancel_on_drop();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        tests::hook(&s, tests::At::Started);
        let (row, enrichment) = {
            let store = s.reader.lock().expect("reader lock");
            // The drawer opens torrents a filter hid (`hidden=true`), so
            // its Related does too; Adult while hidden is a 404.
            visible(&store, &hash, true, adult)?
        };
        let identity = record(enrichment.as_ref()).as_ref().and_then(identity);
        let other_versions = match (&identity, &s.index) {
            (None, _) => HitGroup::unavailable(Unavailable::NoTitle),
            (Some(_), None) => HitGroup::unavailable(Unavailable::SearchDisabled),
            (Some(id), Some(index)) => other_versions(&s, index, row.id, id, adult, &cancel)?,
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

/// The other versions of `identity`, under the request's `cancel`: it's
/// checked before the search and before each candidate's lookups, so a
/// request the client gave up on stops there with `search_cancelled`.
fn other_versions(
    s: &ApiState,
    search: &lodestar_search::SearchHandle,
    id: i64,
    identity: &TitleIdentity,
    adult: bool,
    cancel: &CancelToken,
) -> ApiResult<HitGroup> {
    let index = search.index();
    let node = shown(&candidates_query(identity), adult);
    // Never a `file:(…)` clause, but compiled under the token all the
    // same, so a clause added to `candidates_query` later stays stoppable.
    let compiled = index
        .query_cancelled(&node, unix_now(), 0, cancel)
        .map_err(search::search_error)?;
    cancel.check().map_err(search::search_error)?;
    // Relevance: the title field's BM25 ranks the exact title over
    // longer ones, and the swarm and freshness priors re-rank the first
    // window when hot columns are served (M4).
    let results = index
        .search_page(&compiled, Sort::Relevance, SIBLING_CANDIDATES, 0)
        .map_err(internal)?;
    let mut hits = Vec::new();
    let mut truncated = false;
    for h in &results.hits {
        // Up to `SIBLING_CANDIDATES` lookups, each under the shared
        // reader's lock: the bulk of the work, so checked each time.
        cancel.check().map_err(search::search_error)?;
        #[cfg(test)]
        tests::hook(s, tests::At::Candidate);
        if h.id as i64 == id {
            continue;
        }
        // The shared reader is locked per candidate, not across them.
        let (row, e) = {
            let store = s.reader.lock().expect("reader lock");
            // Deleted since indexing: it drops out.
            let Some(row) = store.torrent(h.id as i64).map_err(internal)? else {
                continue;
            };
            let e = store.enrichment(row.id).map_err(internal)?;
            (row, e)
        };
        // The index may lag the store: the stored state decides.
        if withheld(&row, e.as_ref(), false, adult) {
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
        let swarm = {
            let store = s.reader.lock().expect("reader lock");
            swarm::summary(&store, s.hot.as_deref(), &row).map_err(internal)?
        };
        hits.push(hit_row(&row, h.score, e, swarm));
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
    // "Show adult content", read once for the whole request.
    let adult = s.adult.shown();
    tokio::task::spawn_blocking(move || {
        // The reader is shared by every API read: it's locked for each
        // lookup and each blob, never across the comparison.
        let mut rows = Vec::with_capacity(n);
        let mut swarms = Vec::with_capacity(n);
        for h in &hashes {
            let store = s.reader.lock().expect("reader lock");
            let found = visible(&store, h, false, adult)?;
            swarms.push(swarm::summary(&store, s.hot.as_deref(), &found.0).map_err(internal)?);
            rows.push(found);
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
        let file_diff = file_diff(&s.reader, rows.iter().map(|(r, _)| r.id))?;
        let scale_bytes = rows.iter().map(|(r, _)| r.size).max().unwrap_or(0);
        let torrents = rows
            .into_iter()
            .zip(values)
            .zip(swarms)
            .map(|(((row, e), attributes), swarm)| column(&row, e, attributes, swarm))
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
    swarm: Summary,
) -> CompareTorrent {
    let hit = hit_row(row, 0.0, e, swarm);
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

/// One torrent's file tree, the reader locked only to read it. Gone
/// meanwhile (deleted since the lookup) is a 404, as in detail.
fn tree(reader: &Mutex<Store>, id: i64) -> ApiResult<FileTree> {
    let files = reader.lock().expect("reader lock").files(id);
    files.map_err(internal)?.ok_or_else(not_found)
}

/// The file lists compared by path and size.
///
/// Memory stays bounded however large the trees (up to metainfo's
/// 200,000 files each): one decoded tree is held at a time, and between
/// passes only each torrent's sorted keys (16 bytes a file), so five
/// maximal trees cost about 16 MB. The listed entries are capped at
/// [`DIFF_FILES`] per list.
fn file_diff(reader: &Mutex<Store>, ids: impl Iterator<Item = i64>) -> ApiResult<FileDiff> {
    let ids: Vec<i64> = ids.collect();
    let mut sets: Vec<Vec<Key>> = Vec::with_capacity(ids.len());
    for &id in &ids {
        let tree = tree(reader, id)?;
        let mut keys: Vec<Key> = tree_files(&tree).map(|(k, _, _)| k).collect();
        keys.sort_unstable();
        keys.dedup();
        sets.push(keys);
    }
    let has = |i: usize, k: &Key| sets[i].binary_search(k).is_ok();
    let n = sets.len();
    let common_count = sets[0].iter().filter(|k| (1..n).all(|j| has(j, k))).count() as u64;
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
        let tree = tree(reader, id)?;
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// Where [`hook`] is called from.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum At {
        /// The siblings handler's blocking work begins.
        Started,
        /// A sibling candidate, after its cancel check.
        Candidate,
    }

    type Hook = Box<dyn FnMut(At) + Send>;

    /// Hooks by the address of the state's reader, so tests running at
    /// once (and the blocking pool's threads) each see only their own.
    static HOOKS: Mutex<Vec<(usize, Hook)>> = Mutex::new(Vec::new());

    pub(super) fn hook(s: &ApiState, at: At) {
        let key = Arc::as_ptr(&s.reader) as usize;
        let mut hooks = HOOKS.lock().unwrap_or_else(|e| e.into_inner());
        for (_, f) in hooks.iter_mut().filter(|(k, _)| *k == key) {
            f(at);
        }
    }

    /// Sets `s`'s hook until the returned guard drops.
    fn set_hook(s: &ApiState, f: impl FnMut(At) + Send + 'static) -> impl Drop {
        let key = Arc::as_ptr(&s.reader) as usize;
        HOOKS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((key, Box::new(f)));
        struct Unset(usize);
        impl Drop for Unset {
            fn drop(&mut self) {
                HOOKS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|(k, _)| *k != self.0);
            }
        }
        Unset(key)
    }

    /// One metainfo dict: a single file named after the torrent.
    fn metainfo(name: &str) -> Vec<u8> {
        use lodestar_bencode::{OwnedValue, to_vec};
        let entry = |k: &str, v| (k.as_bytes().to_vec(), v);
        to_vec(&OwnedValue::Dict(
            [
                entry("length", OwnedValue::Int(1 << 30)),
                entry("name", OwnedValue::Bytes(name.as_bytes().to_vec())),
                entry("piece length", OwnedValue::Int(1 << 16)),
                entry("pieces", OwnedValue::Bytes(vec![0; 20])),
            ]
            .into_iter()
            .collect(),
        ))
    }

    /// Five versions of one film, stored, classified and indexed; the
    /// state, the first one's id and its title identity.
    fn versions() -> (ApiState, i64, TitleIdentity, tempfile::TempDir) {
        use lodestar_store::{NewTorrent, SourceId, StoreConfig};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        let raws: Vec<Vec<u8>> = [
            "1080p.BluRay",
            "720p.WEB-DL",
            "2160p.WEB",
            "480p.DVD",
            "BDRip",
        ]
        .iter()
        .map(|q| metainfo(&format!("The.Dark.Knight.2008.{q}.x264-GRP")))
        .collect();
        let infos: Vec<lodestar_metainfo::Info> = raws
            .iter()
            .map(|r| lodestar_metainfo::Info::parse(r).unwrap())
            .collect();
        let batch: Vec<NewTorrent<'_>> = infos
            .iter()
            .zip(&raws)
            .enumerate()
            .map(|(i, (info, raw))| NewTorrent {
                info,
                raw_info: Some(raw),
                discovered_at: 1_700_000_000 + i as i64,
                metadata_at: 1_700_000_001 + i as i64,
                extra_flags: 0,
                source: SourceId::new(0).unwrap(),
            })
            .collect();
        let ids: Vec<i64> = store
            .insert(&batch)
            .unwrap()
            .into_iter()
            .map(|i| i.id())
            .collect();
        let mut enriched = Vec::new();
        for id in &ids {
            let row = store.torrent(*id).unwrap().unwrap();
            let tree = store.files(*id).unwrap().unwrap();
            let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
            let (e, _) = lodestar_filter::reclassify::enrich_corrected(
                &row.name,
                &files,
                lodestar_classify::Correction::default(),
            );
            enriched.push((*id, e));
        }
        store.set_enrichments(&enriched).unwrap();
        let identity = super::record(store.enrichment(ids[0]).unwrap().as_ref())
            .as_ref()
            .and_then(self::identity)
            .expect("a title identity");
        let index = lodestar_search::SearchIndex::in_memory().unwrap();
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(&mut store, 1000).unwrap();
        w.commit().unwrap();
        let state = ApiState {
            auth: Arc::new(crate::Auth::load(&dir.path().join("auth.json"), false).unwrap()),
            index: Some(index.into()),
            reader: Arc::new(Mutex::new(
                Store::open_reader(&db, StoreConfig::default()).unwrap(),
            )),
            filters: None,
            settings: None,
            ops: None,
            user_data: None,
            adult: crate::AdultVisibility::follow(|| false),
            insights: None,
            hot: None,
            events: None,
        };
        (state, ids[0], identity, dir)
    }

    fn run(
        s: &ApiState,
        id: i64,
        identity: &TitleIdentity,
        cancel: &CancelToken,
    ) -> ApiResult<HitGroup> {
        other_versions(s, s.index.as_ref().unwrap(), id, identity, false, cancel)
    }

    fn is_cancelled(r: &ApiResult<HitGroup>) -> bool {
        matches!(r, Err(e) if e.status() == StatusCode::SERVICE_UNAVAILABLE && e.code() == "search_cancelled")
    }

    #[test]
    fn other_versions_are_found_under_a_live_token() {
        let (s, id, identity, _dir) = versions();
        let group = run(&s, id, &identity, &CancelToken::new()).unwrap();
        assert_eq!(group.hits.len(), 4, "{group:?}");
    }

    /// A cancel that lands while candidates are read stops at the next
    /// one, with `search_cancelled`.
    #[test]
    fn a_cancel_during_the_candidates_stops_at_the_next() {
        let (s, id, identity, _dir) = versions();
        let cancel = CancelToken::new();
        let visits = Arc::new(AtomicUsize::new(0));
        let (token, seen) = (cancel.clone(), visits.clone());
        let _hook = set_hook(&s, move |at| {
            if at == At::Candidate {
                seen.fetch_add(1, Ordering::Relaxed);
                token.cancel();
            }
        });
        let r = run(&s, id, &identity, &cancel);
        assert!(is_cancelled(&r), "{r:?}");
        assert_eq!(visits.load(Ordering::Relaxed), 1);
    }

    /// Through the handler: a request abandoned (its future dropped, as
    /// axum drops it when the client disconnects) while its blocking work
    /// waits on the shared reader reads no candidate once it can go on.
    #[tokio::test]
    // Holding the reader across the awaits is the point: it keeps the
    // blocking work waiting until the request is gone. Nothing else in
    // this runtime takes that lock.
    #[allow(clippy::await_holding_lock)]
    async fn an_abandoned_siblings_request_reads_no_candidate() {
        let (s, id, _, _dir) = versions();
        let hash = {
            let store = s.reader.lock().unwrap();
            torrents::hex(&store.torrent(id).unwrap().unwrap().info_hash.0)
        };
        let (started_tx, mut started) = tokio::sync::mpsc::unbounded_channel();
        let candidates = Arc::new(AtomicUsize::new(0));
        let seen = candidates.clone();
        let _hook = set_hook(&s, move |at| match at {
            At::Started => {
                let _ = started_tx.send(());
            }
            At::Candidate => {
                seen.fetch_add(1, Ordering::Relaxed);
            }
        });
        let baseline = Arc::strong_count(&s.reader);
        let held = s.reader.lock().unwrap();
        let request = tokio::spawn(siblings(State(s.clone()), Path(hash)));
        // The handler is past its guard and its work is on the blocking
        // pool, waiting for the reader.
        tokio::time::timeout(Duration::from_secs(30), started.recv())
            .await
            .expect("the siblings work never started");
        request.abort();
        let _ = request.await;
        drop(held);
        // The work ends (dropping its state) once it stops.
        let deadline = Instant::now() + Duration::from_secs(30);
        while Arc::strong_count(&s.reader) > baseline {
            assert!(Instant::now() < deadline, "the siblings work never ended");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(candidates.load(Ordering::Relaxed), 0);
    }

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
        for t in [
            "Spider-Man",
            "Spider Man",
            "SpiderMan",
            "spider.man",
            "SPIDER  MAN",
        ] {
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
