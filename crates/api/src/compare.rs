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
use lodestar_search::{CancelToken, Node, Sort, TitleIdentityParts, title_identity_parts};
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
/// Identical-payload candidates read from the index: torrents with the
/// signature, each then confirmed by its stored file sizes.
const PAYLOAD_CANDIDATES: usize = 200;
/// A torrent's files whose piece roots "the same file elsewhere" looks
/// up, largest first: a torrent with very many files doesn't fan out.
pub const SAME_FILE_ROOTS: usize = 64;
/// Same-file candidates read, at most.
const SAME_FILE_CANDIDATES: usize = 1000;
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

/// A torrent's title identity (`docs/decisions/2026-10-22-B13.md`):
/// the search crate's [`title_identity_parts`], the one definition its
/// `title_identity` term hashes (B13b), so the two can't drift. `None`
/// without a parsed title that has any words.
fn identity(r: &Record) -> Option<TitleIdentity> {
    title_identity_parts(r).map(|p| TitleIdentity {
        absolute: p.absolute,
        episodes: p.episodes,
        seasons: p.seasons,
        title_words: p.title_words,
        years: p.years,
    })
}

/// The `title_identity` term of an identity: its parts, hashed as the
/// index hashes them.
fn identity_key(id: &TitleIdentity) -> u64 {
    TitleIdentityParts {
        title_words: id.title_words.clone(),
        years: id.years.clone(),
        seasons: id.seasons.clone(),
        episodes: id.episodes.clone(),
        absolute: id.absolute.clone(),
    }
    .key()
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
/// identical-payload clones (GS-0's `payload_sig`) and the same file
/// elsewhere (v2 piece roots, v2 and hybrid torrents only). Each group
/// is confirmed against the stored state and withheld as other versions
/// are; the `*_partial` flags say when a backfill means some may be
/// missing.
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
        let own = tree(&s.reader, row.id)?;
        let sizes = content_sizes(&own);
        let (identical_payload, identical_payload_partial) = match (&s.index, sizes.is_empty()) {
            (_, true) => (HitGroup::unavailable(Unavailable::NoContentFiles), false),
            (None, false) => (HitGroup::unavailable(Unavailable::SearchDisabled), false),
            (Some(index), false) => identical_payload(&s, index, row.id, sizes, adult, &cancel)?,
        };
        let roots = file_roots(&own);
        let (same_file, same_file_partial, same_file_capped) = match (&s.index, &roots) {
            (_, Roots::NoContent) => (
                HitGroup::unavailable(Unavailable::NoContentFiles),
                false,
                false,
            ),
            (_, Roots::V1Only) => (
                HitGroup::unavailable(Unavailable::NeedsV2Files),
                false,
                false,
            ),
            (None, Roots::Some { .. }) => (
                HitGroup::unavailable(Unavailable::SearchDisabled),
                false,
                false,
            ),
            (Some(index), Roots::Some { roots, capped }) => {
                let (group, partial, more) = same_file(&s, index, row.id, roots, adult, &cancel)?;
                (group, partial, *capped || more)
            }
        };
        Ok(Json(SiblingsResponse {
            hash: torrents::hex(&row.info_hash.0),
            identical_payload,
            identical_payload_partial,
            identity,
            other_versions,
            same_file,
            same_file_capped,
            same_file_partial,
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
    // The identity's own term narrows the candidates to its versions
    // (B13b): a short title with no year ("Up") matches too many longer
    // ones for the window alone. Where the index can't answer the term
    // (an older index, or a backfill of it running), the window does, as
    // before; either way the stored records decide below.
    let now = unix_now();
    let by_identity = index
        .query_title_identity(&node, identity_key(identity), now, 0, cancel)
        .map_err(search::search_error)?;
    let compiled = match by_identity {
        Some(c) => c,
        None => index
            .query_cancelled(&node, now, 0, cancel)
            .map_err(search::search_error)?,
    };
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

/// A tree's content file sizes (padding left out), sorted: what the
/// payload signature hashes, and what confirms a clone.
fn content_sizes(tree: &FileTree) -> Vec<u64> {
    let mut sizes: Vec<u64> = tree
        .files
        .iter()
        .filter(|f| !f.is_padding())
        .map(|f| f.size)
        .collect();
    sizes.sort_unstable();
    sizes
}

/// Identical-payload clones (§6 "Grouping/dedup"): torrents whose
/// `payload_sig` is this one's, found as `sig:` finds them, each then
/// confirmed by its stored file sizes, so a signature collision never
/// shows. `sizes` are this torrent's ([`content_sizes`]), not empty.
/// Partial (the flag) while the index backfills the field: torrents it
/// hasn't reached yet can't be found.
fn identical_payload(
    s: &ApiState,
    search: &lodestar_search::SearchHandle,
    id: i64,
    sizes: Vec<u64>,
    adult: bool,
    cancel: &CancelToken,
) -> ApiResult<(HitGroup, bool)> {
    let index = search.index();
    let availability = index.availability("payload_sig");
    if availability == lodestar_search::Availability::Missing {
        // An older index serving during a rebuild.
        return Ok((
            HitGroup::unavailable(Unavailable::NeedsPayloadSignature),
            false,
        ));
    }
    let partial = !availability.complete();
    let Some(sig) = lodestar_classify::payload::payload_sig_of_sizes(sizes.clone()) else {
        return Ok((HitGroup::unavailable(Unavailable::NoContentFiles), false));
    };
    let node = shown(
        &Node::Sig {
            sig: lodestar_search::query::sig_hex(sig),
        },
        adult,
    );
    let compiled = index
        .query_cancelled(&node, unix_now(), 0, cancel)
        .map_err(search::search_error)?;
    cancel.check().map_err(search::search_error)?;
    let results = index
        .search_page(&compiled, Sort::Relevance, PAYLOAD_CANDIDATES, 0)
        .map_err(internal)?;
    let ids = results.hits.iter().map(|h| (h.id as i64, h.score));
    let group = confirmed(s, id, ids, adult, cancel, |tree| same_payload(tree, &sizes))?;
    Ok((group, partial))
}

/// Whether a candidate's stored files are the payload `sizes` describe:
/// equal signatures with other sizes are a collision, never a clone.
fn same_payload(tree: &FileTree, sizes: &[u64]) -> bool {
    content_sizes(tree) == sizes
}

/// This torrent's v2 piece roots, for "the same file elsewhere".
enum Roots {
    /// No content files at all.
    NoContent,
    /// Content, but no file carries a piece root: a v1-only torrent.
    V1Only,
    /// The roots of its largest files, distinct, at most
    /// [`SAME_FILE_ROOTS`]; `capped` when it has more.
    Some { roots: Vec<[u8; 32]>, capped: bool },
}

fn file_roots(tree: &FileTree) -> Roots {
    let mut files: Vec<(u64, [u8; 32])> = tree
        .files
        .iter()
        .filter(|f| !f.is_padding())
        .filter_map(|f| f.pieces_root.map(|r| (f.size, r)))
        .collect();
    if files.is_empty() {
        return if tree.files.iter().any(|f| !f.is_padding()) {
            Roots::V1Only
        } else {
            Roots::NoContent
        };
    }
    // Largest first; equal sizes by root, so the choice is stable.
    files.sort_unstable_by(|a, b| b.cmp(a));
    let mut roots: Vec<[u8; 32]> = Vec::new();
    let mut capped = false;
    for (_, r) in files {
        if roots.contains(&r) {
            continue;
        }
        if roots.len() == SAME_FILE_ROOTS {
            capped = true;
            break;
        }
        roots.push(r);
    }
    Roots::Some { roots, capped }
}

/// The same file elsewhere: torrents with a file whose v2 `pieces_root`
/// is one of `roots` (an exact file identity), from the file index, each
/// confirmed by its stored file tree. v1 files carry no root, so only v2
/// and hybrid torrents are found. Partial (the flag) while the file
/// index backfills the field; the last flag when more candidates
/// matched than were read ([`SAME_FILE_CANDIDATES`]).
fn same_file(
    s: &ApiState,
    search: &lodestar_search::SearchHandle,
    id: i64,
    roots: &[[u8; 32]],
    adult: bool,
    cancel: &CancelToken,
) -> ApiResult<(HitGroup, bool, bool)> {
    let index = search.index();
    let availability = index.availability("file.pieces_root");
    if availability == lodestar_search::Availability::Missing {
        return Ok((
            HitGroup::unavailable(Unavailable::NeedsPieceRoots),
            false,
            false,
        ));
    }
    let partial = !availability.complete();
    cancel.check().map_err(search::search_error)?;
    // The newest candidates, at most `SAME_FILE_CANDIDATES` (one more
    // than this torrent itself could take).
    let (ids, more) = index
        .torrents_with_pieces_roots(roots, SAME_FILE_CANDIDATES, cancel)
        .map_err(search::search_error)?;
    let ids = ids.into_iter().rev().map(|t| (t as i64, 0.0));
    let group = confirmed(s, id, ids, adult, cancel, |tree| {
        tree.files
            .iter()
            .filter(|f| !f.is_padding())
            .any(|f| f.pieces_root.is_some_and(|r| roots.contains(&r)))
    })?;
    Ok((group, partial, more))
}

/// The candidates `ids` (with their scores) that the stored state
/// confirms: not `id` itself, still stored, not withheld from this
/// request (B13's rule, as other versions apply it), and passing `keep`
/// on the stored file tree. At most [`MAX_SIBLINGS`]; `truncated` when
/// more pass. `cancel` is checked before each candidate's lookups.
fn confirmed(
    s: &ApiState,
    id: i64,
    ids: impl Iterator<Item = (i64, f32)>,
    adult: bool,
    cancel: &CancelToken,
    keep: impl Fn(&FileTree) -> bool,
) -> ApiResult<HitGroup> {
    let mut hits = Vec::new();
    let mut truncated = false;
    for (cid, score) in ids {
        cancel.check().map_err(search::search_error)?;
        #[cfg(test)]
        tests::hook(s, tests::At::Candidate);
        if cid == id {
            continue;
        }
        let (row, e, tree) = {
            let mut store = s.reader.lock().expect("reader lock");
            let Some(row) = store.torrent(cid).map_err(internal)? else {
                continue;
            };
            let e = store.enrichment(row.id).map_err(internal)?;
            // The index may lag the store: the stored state decides.
            if withheld(&row, e.as_ref(), false, adult) {
                continue;
            }
            let Some(tree) = store.files(row.id).map_err(internal)? else {
                continue;
            };
            (row, e, tree)
        };
        if !keep(&tree) {
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
        hits.push(hit_row(&row, score, e, swarm));
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

    /// A torrent of the corpus: its name, store flags and whether it's
    /// corrected to Adult.
    struct Entry {
        name: String,
        flags: i64,
        adult: bool,
    }

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.to_owned(),
            flags: 0,
            adult: false,
        }
    }

    /// A store of `entries`, enriched, and an index over it, built with
    /// `generations` when given (a build shipping a new generation of a
    /// field, so it's being backfilled).
    fn corpus(
        entries: &[Entry],
        generations: Option<lodestar_search::Generations>,
    ) -> (ApiState, Vec<i64>, tempfile::TempDir) {
        use lodestar_store::{NewTorrent, SourceId, StoreConfig};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        let raws: Vec<Vec<u8>> = entries.iter().map(|e| metainfo(&e.name)).collect();
        let infos: Vec<lodestar_metainfo::Info> = raws
            .iter()
            .map(|r| lodestar_metainfo::Info::parse(r).unwrap())
            .collect();
        let batch: Vec<NewTorrent<'_>> = infos
            .iter()
            .zip(&raws)
            .zip(entries)
            .enumerate()
            .map(|(i, ((info, raw), e))| NewTorrent {
                info,
                raw_info: Some(raw),
                discovered_at: 1_700_000_000 + i as i64,
                metadata_at: 1_700_000_001 + i as i64,
                extra_flags: e.flags,
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
        for (id, e) in ids.iter().zip(entries) {
            let row = store.torrent(*id).unwrap().unwrap();
            let tree = store.files(*id).unwrap().unwrap();
            let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
            let correction = lodestar_classify::Correction {
                adult: e.adult.then_some(true),
                ..Default::default()
            };
            let (en, _) =
                lodestar_filter::reclassify::enrich_corrected(&row.name, &files, correction);
            enriched.push((*id, en));
        }
        store.set_enrichments(&enriched).unwrap();
        let mut index = lodestar_search::SearchIndex::in_memory().unwrap();
        let mut w = index.writer(30 << 20).unwrap();
        while w.catch_up(&mut store, 1000).unwrap() > 0 {}
        w.commit().unwrap();
        drop(w);
        if let Some(g) = generations {
            index = index.with_generations(g).unwrap();
        }
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
        (state, ids, dir)
    }

    /// `id`'s title identity, from its stored record.
    fn identity_of(s: &ApiState, id: i64) -> TitleIdentity {
        let store = s.reader.lock().unwrap();
        super::record(store.enrichment(id).unwrap().as_ref())
            .as_ref()
            .and_then(self::identity)
            .expect("a title identity")
    }

    /// Five versions of one film, stored, classified and indexed; the
    /// state, the first one's id and its title identity.
    fn versions() -> (ApiState, i64, TitleIdentity, tempfile::TempDir) {
        let entries: Vec<Entry> = [
            "1080p.BluRay",
            "720p.WEB-DL",
            "2160p.WEB",
            "480p.DVD",
            "BDRip",
        ]
        .iter()
        .map(|q| entry(&format!("The.Dark.Knight.2008.{q}.x264-GRP")))
        .collect();
        let (state, ids, dir) = corpus(&entries, None);
        let identity = identity_of(&state, ids[0]);
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

    /// More decoys than the candidate window holds: the same one-word
    /// title with a year (another work, by identity), indexed first, so
    /// they tie with the versions on the title phrase and fill the window
    /// ahead of them; then three versions with no year, one hidden by a
    /// filter rule and one Adult when asked.
    fn short_title(title: &str, withheld: bool) -> Vec<Entry> {
        let mut entries: Vec<Entry> = (0..SIBLING_CANDIDATES + 100)
            .map(|i| entry(&format!("{title}.2009.1080p.WEB.x264-D{i}")))
            .collect();
        for (q, flags, adult) in [
            ("1080p.BluRay.x264", 0, false),
            (
                "720p.WEB-DL.x264",
                if withheld {
                    lodestar_store::flags::HIDDEN
                } else {
                    0
                },
                false,
            ),
            ("2160p.WEB.x265", 0, withheld),
            ("480p.DVDRip.XviD", 0, false),
        ] {
            entries.push(Entry {
                name: format!("{title}.{q}-GRP"),
                flags,
                adult,
            });
        }
        entries
    }

    fn found(g: &HitGroup) -> Vec<String> {
        let mut v: Vec<String> = g.hits.iter().map(|h| h.name.clone()).collect();
        v.sort();
        v
    }

    /// B13's known limit closed (B13b): "Up" and "Heat" with more decoys
    /// than the window ("Up 2009", another work) still find their other
    /// versions, by the title-identity term.
    #[test]
    fn short_titles_with_no_year_find_their_versions_past_the_window() {
        for title in ["Up", "Heat"] {
            let (s, ids, _dir) = corpus(&short_title(title, false), None);
            let first = ids[SIBLING_CANDIDATES + 100];
            let id = identity_of(&s, first);
            assert_eq!(id.title_words, [title.to_lowercase()]);
            assert!(id.years.is_empty(), "{id:?}");
            assert_ne!(identity_of(&s, ids[0]), id, "a decoy is another work");
            let got = run(&s, first, &id, &CancelToken::new()).unwrap();
            assert_eq!(
                found(&got),
                [
                    format!("{title}.2160p.WEB.x265-GRP"),
                    format!("{title}.480p.DVDRip.XviD-GRP"),
                    format!("{title}.720p.WEB-DL.x264-GRP"),
                ],
                "{title}"
            );
        }
    }

    /// Found by the term, a hidden or Adult version is still withheld
    /// exactly as before: hidden always, Adult unless it's shown.
    #[test]
    fn withheld_versions_stay_withheld() {
        let (s, ids, _dir) = corpus(&short_title("Up", true), None);
        let first = ids[SIBLING_CANDIDATES + 100];
        let id = identity_of(&s, first);
        let index = s.index.as_ref().unwrap();
        let hidden = other_versions(&s, index, first, &id, false, &CancelToken::new()).unwrap();
        assert_eq!(found(&hidden), ["Up.480p.DVDRip.XviD-GRP"]);
        let adult = other_versions(&s, index, first, &id, true, &CancelToken::new()).unwrap();
        assert_eq!(
            found(&adult),
            ["Up.2160p.WEB.x265-GRP", "Up.480p.DVDRip.XviD-GRP"]
        );
    }

    /// Where the index can't answer the term (a backfill of it running),
    /// the window decides, as before B13b: never "no siblings" for an
    /// unpopulated field. Within the window the versions are found; past
    /// it, B13's known limit is back until the backfill completes.
    #[test]
    fn a_backfilling_index_falls_back_to_the_window() {
        let backfill = || {
            let g = lodestar_search::Generations::current();
            let next = g.get("title_identity").unwrap() + 1;
            Some(g.with("title_identity", next))
        };
        let entries: Vec<Entry> = ["1080p.BluRay", "720p.WEB-DL", "2160p.WEB"]
            .iter()
            .map(|q| entry(&format!("The.Dark.Knight.2008.{q}.x264-GRP")))
            .collect();
        let (s, ids, _dir) = corpus(&entries, backfill());
        let index = s.index.as_ref().unwrap().index();
        assert!(matches!(
            index.availability("title_identity"),
            lodestar_search::Availability::Backfilling { .. }
        ));
        let id = identity_of(&s, ids[0]);
        assert_eq!(
            run(&s, ids[0], &id, &CancelToken::new())
                .unwrap()
                .hits
                .len(),
            2
        );
        let (s, ids, _dir) = corpus(&short_title("Up", false), backfill());
        let first = ids[SIBLING_CANDIDATES + 100];
        let id = identity_of(&s, first);
        let got = run(&s, first, &id, &CancelToken::new()).unwrap();
        // The decoys were inserted first and score as the versions do,
        // so they fill the window by doc order; were that tie-break to
        // change, this side of the test is what would move, not B13b.
        assert!(got.hits.len() < 3, "the window alone: {:?}", found(&got));
    }

    /// One identity rule (B13b): the identity compare shows and compares
    /// is the search crate's, so equal identities are exactly equal
    /// `title_identity` terms over a corpus of names, and the term of the
    /// identity shown is the one indexed.
    #[test]
    fn the_shown_identity_and_the_indexed_term_agree() {
        let names = [
            "Spider-Man.2002.1080p.BluRay.x264-GRP",
            "Spider.Man.2002.720p.WEB-DL-GRP",
            "SpiderMan (2002) 2160p",
            "Spider-Man.2.2004.1080p.BluRay-GRP",
            "Spider-Man.1080p.WEB-GRP",
            "Show.S01.1080p.WEB-GRP",
            "Show.S01E01.1080p.WEB-GRP",
            "Show.S01E01-E03.720p-GRP",
            "Show.E02.1080p-GRP",
            "Up.2009.1080p.BluRay-GRP",
            "Up.1080p.BluRay-GRP",
            "Up.720p.WEB-GRP",
            "Heat.1995.1080p-GRP",
            "Amélie.2001.1080p-GRP",
            "Amelie.2001.720p-GRP",
        ];
        let entries: Vec<Entry> = names.iter().map(|n| entry(n)).collect();
        let (s, ids, _dir) = corpus(&entries, None);
        let records: Vec<Record> = {
            let store = s.reader.lock().unwrap();
            ids.iter()
                .map(|id| super::record(store.enrichment(*id).unwrap().as_ref()).unwrap())
                .collect()
        };
        // The term the indexer writes for a record: what `TorrentDoc`
        // carries, built the way indexing builds it.
        let indexed = |r: &Record| lodestar_search::DocEnrichment::new(r, None).title_identity;
        for a in &records {
            // The key Siblings query by is the key the index holds.
            assert_eq!(identity(a).as_ref().map(identity_key), indexed(a), "{a:?}");
            for b in &records {
                // One identity, one term; two identities, two terms.
                assert_eq!(
                    identity(a) == identity(b),
                    indexed(a) == indexed(b),
                    "{a:?} / {b:?}"
                );
            }
        }
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
        let title_words = |t: &str| {
            identity(&record(&[("title", t)]))
                .map(|i| i.title_words)
                .unwrap_or_default()
        };
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

    // ---- identical payload and the same file (GS-1) ----------------------

    fn file(size: u64, root: Option<u8>, padding: bool) -> lodestar_metainfo::File {
        lodestar_metainfo::File {
            path: vec![format!("f{size}").into_bytes()],
            size,
            attrs: if padding {
                FileAttrs::PADDING
            } else {
                FileAttrs::empty()
            },
            symlink_target: None,
            pieces_root: root.map(|r| [r; 32]),
            hashes: Default::default(),
        }
    }

    fn tree_of(files: Vec<lodestar_metainfo::File>) -> FileTree {
        FileTree {
            encoding: lodestar_metainfo::TextEncoding::UTF8,
            files,
        }
    }

    #[test]
    fn content_sizes_leave_padding_out_and_sort() {
        let t = tree_of(vec![
            file(30, None, false),
            file(5, None, true),
            file(10, None, false),
        ]);
        assert_eq!(content_sizes(&t), [10, 30]);
        assert!(content_sizes(&tree_of(vec![file(5, None, true)])).is_empty());
    }

    #[test]
    fn file_roots_say_what_a_torrent_can_be_looked_up_by() {
        assert!(matches!(file_roots(&tree_of(vec![])), Roots::NoContent));
        assert!(matches!(
            file_roots(&tree_of(vec![file(5, Some(1), true)])),
            Roots::NoContent
        ));
        assert!(matches!(
            file_roots(&tree_of(vec![file(10, None, false)])),
            Roots::V1Only
        ));
        // Largest first, distinct, padding never looked up.
        let t = tree_of(vec![
            file(10, Some(1), false),
            file(30, Some(2), false),
            file(20, Some(2), false),
            file(99, Some(9), true),
            file(40, None, false),
        ]);
        let Roots::Some { roots, capped } = file_roots(&t) else {
            panic!("roots");
        };
        assert_eq!(roots, [[2; 32], [1; 32]]);
        assert!(!capped);
        // More files than are looked up: the largest, and capped said.
        let many: Vec<_> = (0..SAME_FILE_ROOTS as u64 + 5)
            .map(|i| file(1000 + i, Some(i as u8), false))
            .collect();
        let Roots::Some { roots, capped } = file_roots(&tree_of(many)) else {
            panic!("roots");
        };
        assert_eq!(roots.len(), SAME_FILE_ROOTS);
        assert_eq!(roots[0], [(SAME_FILE_ROOTS + 4) as u8; 32]);
        assert!(capped);
    }

    fn payload_of(s: &ApiState, id: i64, cancel: &CancelToken) -> ApiResult<(HitGroup, bool)> {
        let tree = tree(&s.reader, id).unwrap();
        identical_payload(
            s,
            s.index.as_ref().unwrap(),
            id,
            content_sizes(&tree),
            false,
            cancel,
        )
    }

    /// The corpus's torrents are one 1 GiB file each: every one is a clone
    /// of every other.
    #[test]
    fn clones_are_found_and_a_cancel_stops_at_the_next_candidate() {
        let (s, id, _, _dir) = versions();
        let (group, partial) = payload_of(&s, id, &CancelToken::new()).unwrap();
        assert_eq!(group.hits.len(), 4, "{group:?}");
        assert!(!partial);
        let cancel = CancelToken::new();
        let visits = Arc::new(AtomicUsize::new(0));
        let (token, seen) = (cancel.clone(), visits.clone());
        let _hook = set_hook(&s, move |at| {
            if at == At::Candidate {
                seen.fetch_add(1, Ordering::Relaxed);
                token.cancel();
            }
        });
        let r = payload_of(&s, id, &cancel).map(|(g, _)| g);
        assert!(is_cancelled(&r), "{r:?}");
        assert_eq!(visits.load(Ordering::Relaxed), 1);
    }

    /// While the index backfills `payload_sig`, the group says it may be
    /// incomplete: here no document carries it yet, so it finds nothing,
    /// and that's never "none".
    #[test]
    fn a_backfilling_index_marks_clones_partial() {
        let g = lodestar_search::Generations::current();
        let next = g.get("payload_sig").unwrap() + 1;
        let entries = [
            entry("The.Dark.Knight.2008.1080p"),
            entry("Another.Name.2008"),
        ];
        let (s, ids, _dir) = corpus(&entries, Some(g.with("payload_sig", next)));
        let (group, partial) = payload_of(&s, ids[0], &CancelToken::new()).unwrap();
        assert!(partial);
        assert_eq!(group.status, crate::dto::discover::GroupStatus::Available);
    }

    /// A candidate the index returns for a signature whose stored sizes
    /// differ (a collision, played by forging the candidate list) is
    /// dropped by the check `identical_payload` confirms with; the same
    /// candidate for its real payload is kept.
    #[test]
    fn a_signature_collision_is_never_confirmed() {
        let (s, ids, _dir) = corpus(&[entry("Clone.A"), entry("Clone.B")], None);
        let real = content_sizes(&tree(&s.reader, ids[0]).unwrap());
        let collided = vec![real[0] + 1];
        let run = |sizes: &[u64]| {
            let forged = std::iter::once((ids[1], 1.0));
            confirmed(&s, ids[0], forged, false, &CancelToken::new(), |t| {
                same_payload(t, sizes)
            })
            .unwrap()
        };
        assert_eq!(found(&run(&real)), ["Clone.B"]);
        assert!(run(&collided).hits.is_empty());
    }
}
