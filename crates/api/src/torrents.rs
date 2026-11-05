//! `/torrents/{hash}`: one torrent's details and its file list. Display
//! data comes from SQLite, not the index (§8).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use lodestar_metainfo::{FileAttrs, InfoHash, Text};
use lodestar_search::flag_names;
use lodestar_search::query::Network;
use lodestar_store::claims::{Claims, MAX_FILES, MAX_NAME};
use lodestar_store::unverified::StoredClaims;
use lodestar_store::{FileTree, MAX_SOURCES, SourceId, Store, TorrentRow, flags as store_flags};
use utoipa::OpenApi;

use crate::auth::Access;
use crate::dto::error::ErrorEnvelope;
use crate::dto::ops::ConsentClass;
use crate::dto::search::{HitSource, SearchHit};
use crate::dto::torrents::{
    ClaimDifferences, ClaimedNumber, ClaimedText, FileEntry, FilesPage, FilesQuery, Provenance,
    ProvenanceSource, TorrentDetail,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Path, Query};
use crate::ops::OpsApi;
use crate::swarm::{Detail, Summary};
use crate::{ApiState, auth, swarm};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(detail, files))]
pub(crate) struct ApiDoc;

/// Files in the detail response; more via `/files` (§9).
pub const DETAIL_FILES: usize = 500;
const MAX_FILES_PAGE: usize = 1000;

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/torrents/{hash}", get(detail))
            .route("/torrents/{hash}/files", get(files)),
    )
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn parse_hash(s: &str) -> ApiResult<InfoHash> {
    let bad = || {
        ApiError::bad_request("bad_hash", "Expected a 40-character hex info hash.").field("hash")
    };
    if s.len() != 40 {
        return Err(bad());
    }
    let mut out = [0u8; 20];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2).ok_or_else(bad)?, 16).map_err(|_| bad())?;
    }
    Ok(InfoHash(out))
}

/// Who a provenance answer is for (§18.7). Private sources' names and
/// claims go to the admin session only: never to an API key or feed
/// token, and so never to Torznab, feeds, exports or any public route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Viewer {
    /// The signed-in browser, or the trusted proxy's user.
    Admin,
    /// Anyone else: an API key or a feed token.
    Public,
}

impl Viewer {
    /// From the request's [`Access`]: a route behind [`auth::protected`]
    /// has none and is session-only, so it's the admin; a route behind
    /// [`auth::key_or_session`] says who came.
    pub(crate) fn of(access: Option<&Access>) -> Viewer {
        match access {
            None | Some(Access::Session) => Viewer::Admin,
            Some(Access::Key { .. } | Access::Feed { .. }) => Viewer::Public,
        }
    }
}

/// A source plugin of this build, as provenance shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginName {
    pub name: String,
    /// Its consent class isn't `account` (§18.7: Prowlarr, Jackett,
    /// private-tracker announces and your own clients are). Its results
    /// may be public.
    pub public: bool,
}

/// Display names and privacy by manifest id.
pub(crate) type PluginNames = HashMap<String, PluginName>;

/// Manifests are fixed for the life of the process, so each pipeline's
/// list is read once (it takes the writer's lock) and kept while that
/// pipeline lives.
type NamesCache = Vec<(Weak<dyn OpsApi>, Arc<PluginNames>)>;
static PLUGIN_NAMES: Mutex<NamesCache> = Mutex::new(Vec::new());

/// The plugins of this build by manifest id; empty when no pipeline is
/// served or it couldn't list them (then every source shows by id and
/// counts as private, so a public caller sees none).
pub(crate) async fn plugin_names(s: &ApiState) -> Arc<PluginNames> {
    let Some(ops) = s.ops.clone() else {
        return Arc::default();
    };
    let key = Arc::downgrade(&ops);
    {
        let mut cache = PLUGIN_NAMES.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|(w, _)| w.strong_count() > 0);
        if let Some((_, names)) = cache.iter().find(|(w, _)| Weak::ptr_eq(w, &key)) {
            return names.clone();
        }
    }
    let Ok(list) = ops.sources().await else {
        // Not kept: the next request asks again.
        return Arc::default();
    };
    let names: Arc<PluginNames> = Arc::new(
        list.into_iter()
            .map(|src| {
                let public = src.consent != ConsentClass::Account;
                (
                    src.plugin,
                    PluginName {
                        name: src.name,
                        public,
                    },
                )
            })
            .collect(),
    );
    let mut cache = PLUGIN_NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if !cache.iter().any(|(w, _)| Weak::ptr_eq(w, &key)) {
        cache.push((key, names.clone()));
    }
    names
}

/// One registered source, as provenance shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceEntry {
    pub id: u8,
    pub plugin: String,
    pub name: String,
    /// See [`PluginName::public`]; `None` for a plugin this build doesn't
    /// run (or no pipeline is served), whose class isn't known: a public
    /// caller sees none of those.
    pub public: Option<bool>,
}

/// The store's sources by id, named from [`plugin_names`].
#[derive(Debug, Clone, Default)]
pub(crate) struct Catalogue {
    by_id: Vec<Option<SourceEntry>>,
}

impl Catalogue {
    pub(crate) fn read(store: &Store, names: &PluginNames) -> lodestar_store::Result<Catalogue> {
        let mut by_id = vec![None; usize::from(MAX_SOURCES)];
        for row in store.sources()? {
            let known = names.get(&row.plugin);
            by_id[usize::from(row.id.get())] = Some(SourceEntry {
                id: row.id.get(),
                name: known.map_or_else(|| row.plugin.clone(), |n| n.name.clone()),
                public: known.map(|n| n.public),
                plugin: row.plugin,
            });
        }
        Ok(Catalogue { by_id })
    }

    /// A catalogue of `entries`, for tests.
    #[cfg(test)]
    pub(crate) fn of(entries: Vec<SourceEntry>) -> Catalogue {
        let mut by_id = vec![None; usize::from(MAX_SOURCES)];
        for e in entries {
            let i = usize::from(e.id);
            by_id[i] = Some(e);
        }
        Catalogue { by_id }
    }

    pub(crate) fn get(&self, id: u8) -> Option<&SourceEntry> {
        self.by_id.get(usize::from(id)).and_then(Option::as_ref)
    }

    /// The source with manifest id `plugin`.
    pub(crate) fn by_plugin(&self, plugin: &str) -> Option<&SourceEntry> {
        self.by_id.iter().flatten().find(|e| e.plugin == plugin)
    }

    /// Whether `viewer` may see the name `plugin` (a `source` facet
    /// value): always the admin; anyone else only a known public source.
    pub(crate) fn shows_plugin(&self, plugin: &str, viewer: Viewer) -> bool {
        viewer == Viewer::Admin
            || self
                .by_plugin(plugin)
                .is_some_and(|e| e.public == Some(true))
    }

    /// The ids in `bits`, the first source first, then by id.
    fn ordered(bits: u64, first: Option<SourceId>) -> impl Iterator<Item = u8> {
        let first = first.map(SourceId::get).filter(|f| bits & (1u64 << f) != 0);
        first
            .into_iter()
            .chain((0..MAX_SOURCES).filter(move |i| bits & (1u64 << i) != 0 && Some(*i) != first))
    }

    /// `row`'s sources `viewer` may see, the first first, and whether
    /// any was left out. A `private_source` torrent's are all private.
    fn visible<'a>(
        &'a self,
        row: &TorrentRow,
        private_claims: impl Fn(u8) -> bool,
        viewer: Viewer,
    ) -> (Vec<(&'a SourceEntry, bool)>, bool) {
        let private_item = row.flags & store_flags::PRIVATE_SOURCE != 0;
        let mut shown = Vec::new();
        let mut withheld = false;
        for id in Self::ordered(row.sources, row.first_source) {
            // A bit with no source row isn't one the store wrote.
            let Some(e) = self.get(id) else { continue };
            // Known to be private, which the admin is told.
            let private = private_item || e.public == Some(false) || private_claims(id);
            if (private || e.public.is_none()) && viewer == Viewer::Public {
                withheld = true;
            } else {
                shown.push((e, private));
            }
        }
        (shown, withheld)
    }

    /// A hit's source: the first one `viewer` may see.
    pub(crate) fn hit_source(&self, row: &TorrentRow, viewer: Viewer) -> Option<HitSource> {
        // Claims aren't read for a hit: a public source's own bit is
        // public, and a private source isn't `public` anyway.
        let (shown, _) = self.visible(row, |_| false, viewer);
        shown.first().map(|(e, _)| HitSource {
            name: e.name.clone(),
            plugin: e.plugin.clone(),
        })
    }

    /// The detail view's provenance of `row`, from its stored claims.
    pub(crate) fn provenance(
        &self,
        row: &TorrentRow,
        claims: &[StoredClaims],
        viewer: Viewer,
    ) -> Provenance {
        let claim_of = |id: u8| claims.iter().find(|c| c.source.get() == id);
        let (shown, mut withheld) = self.visible(
            row,
            |id| claim_of(id).is_some_and(|c| c.private_source),
            viewer,
        );
        let first = row.first_source.map(SourceId::get);
        let sources = shown
            .into_iter()
            .map(|(e, private)| ProvenanceSource {
                claim: claim_of(e.id)
                    .and_then(|c| c.claims.as_ref())
                    .map(|c| differences(row, c)),
                first: first == Some(e.id),
                first_seen_at: (first == Some(e.id)).then_some(row.discovered_at),
                id: e.id,
                name: e.name.clone(),
                plugin: e.plugin.clone(),
                private,
            })
            .collect();
        // Claims from a source that's no longer in the bitset (purged
        // since) aren't shown; to a public caller, nor is their existence.
        if viewer == Viewer::Public && claims.iter().any(|c| c.private_source) {
            withheld = true;
        }
        Provenance {
            network: Network::Bt.as_str().to_owned(),
            sources,
            verified: true,
            withheld,
        }
    }
}

/// `s` cut to [`MAX_NAME`] bytes on a character boundary.
fn bounded(s: &str) -> String {
    let mut end = s.len().min(MAX_NAME);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

/// What `c` claims that `row`'s verified metadata contradicts (§18.5).
pub(crate) fn differences(row: &TorrentRow, c: &Claims) -> ClaimDifferences {
    let name = c
        .name
        .as_deref()
        .filter(|n| *n != row.name)
        .map(|n| ClaimedText {
            claimed: bounded(n),
            verified: bounded(&row.name),
        });
    let size = c.size.filter(|s| *s != row.size).map(|s| ClaimedNumber {
        claimed: s,
        verified: row.size,
    });
    let claimed = c.files.len() as u64;
    // A list cut at the limit says "at least this many".
    let cut = claimed >= MAX_FILES as u64 && row.file_count >= claimed;
    let file_count = (claimed > 0 && claimed != row.file_count && !cut).then_some(ClaimedNumber {
        claimed,
        verified: row.file_count,
    });
    ClaimDifferences {
        matches: name.is_none() && size.is_none() && file_count.is_none(),
        file_count,
        name,
        size,
    }
}

fn flags(row: &TorrentRow) -> Vec<String> {
    flag_names(row.flags).map(str::to_string).collect()
}

/// A search result row. The classification fields are filled from the
/// stored enrichment by `search::enrich`.
pub(crate) fn hit(
    row: &TorrentRow,
    score: f32,
    matched_files: Option<Vec<u64>>,
    swarm: Summary,
) -> SearchHit {
    SearchHit {
        adult_kind: None,
        completed: swarm.completed,
        composition: Vec::new(),
        content_type: None,
        discovered_at: row.discovered_at,
        files: row.file_count,
        flags: flags(row),
        hash: hex(&row.info_hash.0),
        hash_v2: row.info_hash_v2.map(|h| hex(&h.0)),
        id: row.id,
        junk_reasons: Vec::new(),
        junk_score: None,
        last_scrape_at: swarm.last_scrape_at,
        last_seen_at: swarm.last_seen_at,
        leechers: swarm.leechers,
        matched_files,
        metadata_at: row.metadata_at,
        name: row.name.clone(),
        piece_len: row.piece_len,
        resolved_via: None,
        // As `json!` widened it.
        score: f64::from(score),
        seeders: swarm.seeders,
        seeders_approx: swarm.seeders_approx,
        size: row.size,
        source: None,
        swarm_suspect: swarm.swarm_suspect,
        title: None,
        trackers_checked: swarm.trackers_checked,
        trackers_reporting: swarm.trackers_reporting,
        verified: None,
    }
}

fn file_entry(tree: &FileTree, f: &lodestar_metainfo::File) -> FileEntry {
    let path: Vec<String> = f
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
    let mut attrs = Vec::new();
    for (bit, name) in [
        (FileAttrs::PADDING, "padding"),
        (FileAttrs::HIDDEN, "hidden"),
        (FileAttrs::EXECUTABLE, "executable"),
        (FileAttrs::SYMLINK, "symlink"),
    ] {
        if f.attrs.contains(bit) {
            attrs.push(name.to_string());
        }
    }
    FileEntry {
        attrs,
        path: path.join("/"),
        size: f.size,
    }
}

/// Reads a torrent and its files by any swarm hash. An Adult torrent
/// while "Show adult content" is off is a 404 `adult_hidden`, so a
/// pasted hash doesn't get round the preference (§20.9).
fn load(s: &ApiState, hash: &str) -> ApiResult<(TorrentRow, FileTree)> {
    let hash = parse_hash(hash)?;
    let mut store = s.reader.lock().expect("reader lock");
    let not_found = || {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "torrent_not_found",
            "There's no torrent with that hash.",
        )
    };
    let id = store.find(&hash).map_err(internal)?.ok_or_else(not_found)?;
    if !s.adult.shown()
        && store
            .enrichment(id)
            .map_err(internal)?
            .is_some_and(|e| crate::search::is_adult(&e))
    {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "adult_hidden",
            "This torrent is Adult, which is hidden: turn on Show adult content in Settings to see it.",
        ));
    }
    let row = store.torrent(id).map_err(internal)?.ok_or_else(not_found)?;
    let tree = store.files(id).map_err(internal)?.ok_or_else(not_found)?;
    Ok((row, tree))
}

/// The detail view: the summary, the swarm section and the first files.
fn detail_of(
    row: &TorrentRow,
    tree: &FileTree,
    swarm: Summary,
    more: Detail,
    provenance: Provenance,
) -> TorrentDetail {
    TorrentDetail {
        completed: swarm.completed,
        dht_peers: more.dht_peers,
        dht_seeds: more.dht_seeds,
        discovered_at: row.discovered_at,
        file_list: tree
            .files
            .iter()
            .take(DETAIL_FILES)
            .map(|f| file_entry(tree, f))
            .collect(),
        file_list_total: tree.files.len(),
        files: row.file_count,
        flags: flags(row),
        hash: hex(&row.info_hash.0),
        hash_v2: row.info_hash_v2.map(|h| hex(&h.0)),
        id: row.id,
        last_scrape_at: swarm.last_scrape_at,
        last_seen_at: swarm.last_seen_at,
        leechers: swarm.leechers,
        magnet: format!(
            "magnet:?xt=urn:btih:{}&dn={}",
            hex(&row.info_hash.0),
            percent_encode(&row.name)
        ),
        metadata_at: row.metadata_at,
        name: row.name.clone(),
        piece_len: row.piece_len,
        provenance,
        seeders: swarm.seeders,
        seeders_approx: swarm.seeders_approx,
        size: row.size,
        swarm_history: more.swarm_history,
        swarm_suspect: swarm.swarm_suspect,
        swarm_trackers: more.swarm_trackers,
        swarm_trend: more.swarm_trend,
        trackers_checked: swarm.trackers_checked,
        trackers_reporting: swarm.trackers_reporting,
    }
}

/// One torrent by any of its swarm hashes.
#[utoipa::path(
    operation_id = "getTorrent",
    get,
    path = "/torrents/{hash}",
    tag = "torrents",
    params(("hash" = String, Path, description = "A 40-character hex info hash.")),
    responses(
        (status = 200, body = TorrentDetail),
        (status = 404, body = ErrorEnvelope, description = "`torrent_not_found`, or `adult_hidden`: an Adult torrent while \"Show adult content\" is off."),
    )
)]
pub(crate) async fn detail(
    State(s): State<ApiState>,
    access: Option<Extension<Access>>,
    Path(hash): Path<String>,
) -> ApiResult<Json<TorrentDetail>> {
    let viewer = Viewer::of(access.as_ref().map(|a| &a.0));
    let names = plugin_names(&s).await;
    tokio::task::spawn_blocking(move || {
        let (row, tree) = load(&s, &hash)?;
        // Opening a torrent is interest: scrape it soon (§15).
        if let Some(e) = &s.events {
            e.interest(row.id);
        }
        let (summary, more, provenance) = {
            let store = s.reader.lock().expect("reader lock");
            let summary = swarm::summary(&store, s.hot.as_deref(), &row).map_err(internal)?;
            let more = swarm::detail(&store, &summary, row.id).map_err(internal)?;
            let catalogue = Catalogue::read(&store, &names).map_err(internal)?;
            let claims = store.torrent_claims(row.id).map_err(internal)?;
            (summary, more, catalogue.provenance(&row, &claims, viewer))
        };
        Ok(Json(detail_of(&row, &tree, summary, more, provenance)))
    })
    .await
    .map_err(internal)?
}

/// A page of a torrent's file list.
#[utoipa::path(
    operation_id = "listTorrentFiles",
    get,
    path = "/torrents/{hash}/files",
    tag = "torrents",
    params(
        ("hash" = String, Path, description = "A 40-character hex info hash."),
        FilesQuery,
    ),
    responses(
        (status = 200, body = FilesPage),
        (status = 404, body = ErrorEnvelope, description = "`torrent_not_found`, or `adult_hidden`: an Adult torrent while \"Show adult content\" is off."),
    )
)]
pub(crate) async fn files(
    State(s): State<ApiState>,
    Path(hash): Path<String>,
    Query(p): Query<FilesQuery>,
) -> ApiResult<Json<FilesPage>> {
    tokio::task::spawn_blocking(move || {
        let (_, tree) = load(&s, &hash)?;
        let limit = p.limit.unwrap_or(MAX_FILES_PAGE).clamp(1, MAX_FILES_PAGE);
        let files = tree
            .files
            .iter()
            .skip(p.offset)
            .take(limit)
            .map(|f| file_entry(&tree, f))
            .collect();
        Ok(Json(FilesPage {
            files,
            offset: p.offset,
            total: tree.files.len(),
        }))
    })
    .await
    .map_err(internal)?
}

pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use lodestar_store::claims::ClaimedFile;

    use super::*;

    fn row(sources: u64, first: Option<u8>, flags: i64) -> TorrentRow {
        TorrentRow {
            id: 7,
            info_hash: InfoHash([1; 20]),
            info_hash_v2: None,
            name: "Some.Show.S01.1080p".into(),
            name_raw: None,
            size: 4_000,
            piece_len: 1 << 14,
            piece_count: 1,
            file_count: 3,
            info_size: 100,
            flags,
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_001,
            last_seen_at: 1_700_000_002,
            sources,
            first_source: first.and_then(SourceId::new),
        }
    }

    /// `dht` (0, public), `prowlarr` (1, account class: private), `nyaa`
    /// (2, public) and `gone` (3, a plugin this build doesn't run).
    pub(crate) fn catalogue() -> Catalogue {
        Catalogue::of(
            [
                (0u8, "dht", "Mainline DHT", Some(true)),
                (1, "prowlarr", "Prowlarr", Some(false)),
                (2, "nyaa", "Nyaa", Some(true)),
                (3, "gone", "gone", None),
            ]
            .into_iter()
            .map(|(id, plugin, name, public)| SourceEntry {
                id,
                plugin: plugin.into(),
                name: name.into(),
                public,
            })
            .collect(),
        )
    }

    fn claims(source: u8, private: bool, c: Claims) -> StoredClaims {
        StoredClaims {
            torrent_id: 7,
            source: SourceId::new(source).unwrap(),
            private_source: private,
            claims: Some(c),
        }
    }

    fn plugins(p: &Provenance) -> Vec<&str> {
        p.sources.iter().map(|s| s.plugin.as_str()).collect()
    }

    #[test]
    fn the_viewer_is_the_admin_only_with_a_session() {
        assert_eq!(Viewer::of(None), Viewer::Admin);
        assert_eq!(Viewer::of(Some(&Access::Session)), Viewer::Admin);
        assert_eq!(
            Viewer::of(Some(&Access::Key {
                id: 1,
                scopes: crate::keys::scope::ALL
            })),
            Viewer::Public
        );
        assert_eq!(
            Viewer::of(Some(&Access::Feed {
                id: 1,
                saved_search_id: None
            })),
            Viewer::Public
        );
    }

    #[test]
    fn sources_are_listed_first_found_first_then_by_id() {
        let c = catalogue();
        // dht, nyaa and an unregistered bit 9; nyaa found it first.
        let r = row(0b101 | 1 << 9, Some(2), 0);
        let p = c.provenance(&r, &[], Viewer::Admin);
        assert_eq!(plugins(&p), ["nyaa", "dht"]);
        assert!(p.sources[0].first && !p.sources[1].first);
        assert_eq!(p.sources[0].first_seen_at, Some(r.discovered_at));
        assert_eq!(p.sources[1].first_seen_at, None);
        assert_eq!(p.sources[0].name, "Nyaa");
        assert!(p.verified && !p.withheld && p.network == "bt");
        // A torrent stored before provenance has none.
        assert!(
            c.provenance(&row(0, None, 0), &[], Viewer::Admin)
                .sources
                .is_empty()
        );
    }

    #[test]
    fn private_sources_and_claims_reach_the_admin_only() {
        let c = catalogue();
        // dht, prowlarr, nyaa (with private claims) and gone.
        let r = row(0b1111, Some(1), 0);
        let stored = [
            claims(1, true, Claims::default()),
            claims(
                2,
                true,
                Claims {
                    name: Some("leaked".into()),
                    ..Claims::default()
                },
            ),
        ];
        let admin = c.provenance(&r, &stored, Viewer::Admin);
        assert_eq!(plugins(&admin), ["prowlarr", "dht", "nyaa", "gone"]);
        let private: Vec<bool> = admin.sources.iter().map(|s| s.private).collect();
        // Unknown isn't known to be private, but stays from the public.
        assert_eq!(private, [true, false, true, false]);
        assert!(!admin.withheld);
        assert!(admin.sources[2].claim.as_ref().unwrap().name.is_some());

        let public = c.provenance(&r, &stored, Viewer::Public);
        assert_eq!(plugins(&public), ["dht"]);
        assert!(public.withheld);
        let text = serde_json::to_string(&public).unwrap();
        for hidden in ["prowlarr", "Prowlarr", "nyaa", "leaked", "gone"] {
            assert!(!text.contains(hidden), "{hidden} in {text}");
        }
        // Its hit shows the first source the caller may see.
        assert_eq!(c.hit_source(&r, Viewer::Admin).unwrap().plugin, "prowlarr");
        assert_eq!(c.hit_source(&r, Viewer::Public).unwrap().plugin, "dht");
    }

    #[test]
    fn a_private_source_torrent_shows_no_source_to_the_public() {
        let c = catalogue();
        // Even public sources' bits: the flag says it was found only
        // through private ones (§18.7).
        let r = row(0b101, Some(0), store_flags::PRIVATE_SOURCE);
        let public = c.provenance(&r, &[], Viewer::Public);
        assert!(public.sources.is_empty() && public.withheld);
        assert_eq!(c.hit_source(&r, Viewer::Public), None);
        let admin = c.provenance(&r, &[], Viewer::Admin);
        assert!(admin.sources.iter().all(|s| s.private));
        assert_eq!(c.hit_source(&r, Viewer::Admin).unwrap().plugin, "dht");
    }

    #[test]
    fn a_private_claim_from_a_purged_source_is_still_withheld() {
        let c = catalogue();
        let r = row(0b1, Some(0), 0);
        let stored = [claims(2, true, Claims::default())];
        assert!(c.provenance(&r, &stored, Viewer::Public).withheld);
        let admin = c.provenance(&r, &stored, Viewer::Admin);
        assert_eq!(plugins(&admin), ["dht"]);
    }

    #[test]
    fn plugin_names_follow_the_class() {
        let c = catalogue();
        for p in ["dht", "prowlarr", "nyaa", "gone", "never-registered"] {
            assert!(c.shows_plugin(p, Viewer::Admin));
        }
        assert!(c.shows_plugin("dht", Viewer::Public));
        assert!(c.shows_plugin("nyaa", Viewer::Public));
        for p in ["prowlarr", "gone", "never-registered"] {
            assert!(!c.shows_plugin(p, Viewer::Public), "{p}");
        }
    }

    #[test]
    fn differences_name_only_what_disagrees() {
        let r = row(0b1, Some(0), 0);
        let same = Claims {
            name: Some(r.name.clone()),
            size: Some(r.size),
            files: (0..3)
                .map(|i| ClaimedFile {
                    path: format!("f{i}"),
                    size: None,
                })
                .collect(),
            ..Claims::default()
        };
        let d = differences(&r, &same);
        assert!(d.matches && d.name.is_none() && d.size.is_none() && d.file_count.is_none());
        // Nothing stated is nothing to disagree with.
        assert!(differences(&r, &Claims::default()).matches);

        let long = "é".repeat(MAX_NAME);
        let other = Claims {
            name: Some(long.clone()),
            size: Some(5),
            files: vec![ClaimedFile {
                path: "x".into(),
                size: None,
            }],
            ..Claims::default()
        };
        let d = differences(&r, &other);
        assert!(!d.matches);
        let name = d.name.unwrap();
        assert!(name.claimed.len() <= MAX_NAME && long.starts_with(&name.claimed));
        assert_eq!(name.verified, r.name);
        assert_eq!(
            d.size,
            Some(ClaimedNumber {
                claimed: 5,
                verified: 4_000
            })
        );
        assert_eq!(
            d.file_count,
            Some(ClaimedNumber {
                claimed: 1,
                verified: 3
            })
        );
    }

    #[test]
    fn a_claimed_list_cut_at_the_limit_is_at_least_that_many() {
        let mut r = row(0b1, Some(0), 0);
        r.file_count = MAX_FILES as u64 + 50;
        let cut = Claims {
            files: vec![
                ClaimedFile {
                    path: "x".into(),
                    size: None,
                };
                MAX_FILES
            ],
            ..Claims::default()
        };
        assert!(differences(&r, &cut).file_count.is_none());
        r.file_count = 10;
        assert!(differences(&r, &cut).file_count.is_some());
    }
}
