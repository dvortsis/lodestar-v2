//! Every plugin kind that finds keys, run on its own recorded fixture
//! with no network, and a relay that hands those finds, unchanged, to a
//! real pipeline. Shared by `tests/safety.rs` (SRC-G) and
//! `bench/tests/sources_overhead.rs`, which include it by path and define
//! `PLUGINS_DIR`, this crate's folder.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lodestar_integrations::clients::{BasicAuth, ClientConfig};
use lodestar_integrations::secret::Secret;
use lodestar_sources::testing::{Exchange, FakeCtx, Replayer, parse_exchange, wait_for};
use lodestar_sources::{
    BoxFuture, Consent, Discovery, Found, Manifest, PluginError, SourceCtx, SourcePlugin,
};
use lodestar_sources_plugins::full_scrape::{FullScrape, StaticPrivateTrackers, TokioClock};
use lodestar_sources_plugins::prowlarr::{Config as ProwlarrConfig, Manager, Prowlarr};
use lodestar_sources_plugins::{
    Credentials, StaticCredentials, academic, archive_org, arr, clients, full_scrape, nyaa,
};
use lodestar_store::SourceId;

use super::PLUGINS_DIR;

// ---------------------------------------------------------------------
// Fixture replay: every plugin kind that finds keys, on its own recorded
// exchanges.
// ---------------------------------------------------------------------

/// One plugin kind's fixture run: the manifest its finds carry and what
/// it found.
pub struct Run {
    pub kind: &'static str,
    pub manifest: Manifest,
    pub finds: Vec<Found>,
}

pub fn fixture(plugin: &str, case: &str) -> PathBuf {
    PathBuf::from(PLUGINS_DIR)
        .join("fixtures")
        .join(plugin)
        .join(case)
}

/// Runs `p` against `replayer` until it has made `requests` requests and
/// sent `finds` discoveries, then stops it (within 1 s, §18.9).
async fn replay<P: SourcePlugin + 'static>(
    p: Arc<P>,
    replayer: Arc<Replayer>,
    requests: usize,
    finds: usize,
) -> (Manifest, Vec<Discovery>) {
    let manifest = p.manifest().clone();
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(3).unwrap(), replayer.clone());
    let running = ctx.start(p);
    let mut got = Vec::new();
    wait_for("the recorded requests", || {
        got.extend(ctx.drain());
        replayer.requests().len() >= requests
    })
    .await;
    wait_for("the discoveries", || {
        got.extend(ctx.drain());
        got.len() >= finds
    })
    .await;
    running.stop_within().await.unwrap();
    got.extend(ctx.drain());
    replayer.finish().unwrap();
    (manifest, got)
}

fn archive_clock() -> i64 {
    1_793_707_200
}

/// A full scrape listing `hashes` (each a repeated byte), in the shape the
/// plugin's own tests record.
fn scrape_reply(hashes: &[u8]) -> Exchange {
    let mut body = b"d5:filesd".to_vec();
    for h in hashes {
        body.extend_from_slice(b"20:");
        body.extend_from_slice(&[*h; 20]);
        body.extend_from_slice(b"d8:completei5e10:downloadedi50e10:incompletei1ee");
    }
    body.extend_from_slice(b"ee");
    parse_exchange(
        "GET http://tracker.example/scrape\n\nHTTP 200\nContent-Type: text/plain\n",
        body,
        Vec::new(),
    )
    .unwrap()
}

fn arr_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture("arr", name)).unwrap()
}

/// Every plugin kind that finds keys, run on its normal fixture. The
/// enrichment kinds (torrent cache, ThePornDB, StashDB) answer lookups
/// about keys already found; their blocklist paths are covered where they
/// live (`docs/decisions/2026-11-07-SRC-G.md`).
async fn every_plugin_kind() -> Vec<Run> {
    let mut runs = Vec::new();
    let mut add = |kind, (manifest, got): (Manifest, Vec<Discovery>)| {
        runs.push(Run {
            kind,
            manifest,
            finds: got.into_iter().map(|d| d.found).collect(),
        })
    };
    let load = |p, c| Arc::new(Replayer::load(&fixture(p, c)).unwrap());

    let got = replay(
        Arc::new(academic::Academic::new()),
        load("academic", "academic_rss"),
        2,
        10,
    )
    .await;
    add("academic", got);

    let got = replay(
        Arc::new(archive_org::ArchiveOrg::with_clock(archive_clock)),
        load("archive_org", "archive_normal"),
        2,
        3,
    )
    .await;
    add("archive_org", got);

    let got = replay(Arc::new(nyaa::Nyaa::new()), load("nyaa", "nyaa_rss"), 2, 6).await;
    add("nyaa", got);

    let got = replay(
        Arc::new(nyaa::Sukebei::new()),
        load("sukebei", "sukebei_rss"),
        2,
        6,
    )
    .await;
    add("sukebei", got);

    let key: Arc<dyn Credentials> = Arc::new(StaticCredentials::new([("api_key", "REDACTED")]));
    let p = Arc::new(Prowlarr::new(
        ProwlarrConfig::new("http://prowlarr.example:9696", Manager::Prowlarr, Some(2)).unwrap(),
        key,
    ));
    let replayer =
        Arc::new(Replayer::load_unordered(&fixture("prowlarr", "prowlarr_rss")).unwrap());
    let got = replay(p, replayer, 6, 5).await;
    add("prowlarr", got);

    let qbit = Arc::new(clients::Clients::new(ClientConfig::QBittorrent {
        url: "http://qbittorrent.example:8080/".into(),
        username: "REDACTED".into(),
        password: Secret::new("REDACTED"),
    }));
    let got = replay(qbit, load("clients", "qbittorrent_poll"), 10, 4).await;
    add("clients/qbittorrent", got);

    let transmission = Arc::new(clients::Clients::new(ClientConfig::Transmission {
        url: "http://transmission.example:9091/".into(),
        auth: Some(BasicAuth {
            username: "REDACTED".into(),
            password: Secret::new("REDACTED"),
        }),
    }));
    let got = replay(transmission, load("clients", "transmission_poll"), 3, 3).await;
    add("clients/transmission", got);

    // A full scrape is keys only (with the tracker as a claim), inside its
    // night window: the clock starts at 01:00 UTC.
    let midnight = 1_800_000_000 - 1_800_000_000 % 86_400;
    let p = Arc::new(FullScrape::new(
        full_scrape::Config::new(&["http://tracker.example/announce"]).unwrap(),
        Arc::new(StaticPrivateTrackers::none()),
        Arc::new(TokioClock::starting_at(midnight + 3600)),
    ));
    let got = replay(
        p,
        Arc::new(Replayer::new(vec![scrape_reply(&[0x51, 0x52, 0x53])])),
        1,
        3,
    )
    .await;
    add("full_scrape", got);

    // Webhooks are pushed to Lodestar, not polled: the plugin's own
    // mapping, on its recorded bodies.
    let got: Vec<Discovery> = [
        (arr::App::Sonarr, "sonarr_grab.json"),
        (arr::App::Radarr, "radarr_grab.json"),
        (arr::App::Sonarr, "sonarr_download.json"),
    ]
    .into_iter()
    .map(|(app, f)| (app, arr_fixture(f)))
    // A third key for the clean control: the download body, recorded for
    // another torrent (the fixtures repeat two keys).
    .chain([(arr::App::Sonarr, {
        let body = String::from_utf8(arr_fixture("sonarr_download.json")).unwrap();
        body.replace(
            "FB9FF1EF824D61BD83B24FB91811804DB71C0C53",
            "0123456789ABCDEF0123456789ABCDEF01234567",
        )
        .replace(
            "fb9ff1ef824d61bd83b24fb91811804db71c0c53",
            "0123456789abcdef0123456789abcdef01234567",
        )
        .into_bytes()
    })])
    .filter_map(|(app, body)| arr::to_found(&arr::parse(app, &body).unwrap()))
    .map(|found| Discovery {
        source: SourceId::new(3).unwrap(),
        found,
        at: 0,
        reply: false,
    })
    .collect();
    add("arr", (arr::ArrWebhooks::new().manifest().clone(), got));
    runs
}

/// The runs, collected on a paused clock of their own (the plugins' poll
/// intervals are minutes to hours) before the pipeline starts on real time.
pub fn fixture_runs() -> Vec<Run> {
    std::thread::spawn(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(every_plugin_kind())
    })
    .join()
    .unwrap()
}

// ---------------------------------------------------------------------
// The relay: a plugin's recorded finds, into a real pipeline.
// ---------------------------------------------------------------------

/// Sends its finds in rounds: each `go` releases the next round.
pub struct Relay {
    manifest: Manifest,
    rounds: Mutex<Option<tokio::sync::mpsc::Receiver<Vec<Found>>>>,
}

pub fn relay(manifest: &Manifest) -> (Arc<Relay>, tokio::sync::mpsc::Sender<Vec<Found>>) {
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let manifest = Manifest {
        consent: Consent::None,
        enabled_by_default: true,
        ..manifest.clone()
    };
    let r = Relay {
        manifest,
        rounds: Mutex::new(Some(rx)),
    };
    (Arc::new(r), tx)
}

impl SourcePlugin for Relay {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        let mut rx = self.rounds.lock().unwrap().take().expect("run once");
        Box::pin(async move {
            loop {
                tokio::select! {
                    _ = ctx.shutdown.requested() => return Ok(()),
                    Some(round) = rx.recv() => {
                        for f in round {
                            ctx.emitter
                                .emit(f)
                                .await
                                .map_err(|_| PluginError("closed".into()))?;
                        }
                    }
                }
            }
        })
    }
}
