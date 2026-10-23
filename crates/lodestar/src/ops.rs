//! The API's view of the running process (`lodestar_api::ops::OpsApi`):
//! `/stats`, `/crawler`, `/sources` and `/network/check` over the
//! pipeline's [`Control`] and [`Monitor`] and the index's counters.
//!
//! Every number is read from the counters `/metrics` and the status page
//! read (`metrics::render`): nothing is counted a second time.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lodestar_api::ops::{
    BoxFuture, ConsentClass, CrawlerRuntime, DhtStatus, ExternalAddress, FetchStatus, NetworkCheck,
    OpsApi, OpsError, PortMapping, ReachReason, RiskLevel, Source, SourceHealth, SourceKind,
    SourcePatch, SourcePriority, SourceState, StatsResponse,
};
use lodestar_pipeline::Monitor;
use lodestar_pipeline::control::{
    self, Control, REACH_MIN_SECS, REACH_WINDOW_SECS, SourceChange, SourceError, SourceView,
};
use lodestar_sources::{Consent, Kind, Priority, Risk};
use lodestar_store::SourceId;

use crate::http::SearchState;
use crate::settings::crawler::Crawler;

/// What a source's `PATCH` changes now, and what waits for v0.6.
const EDITABLE: [&str; 2] = ["enabled", "priority"];
const NOT_YET_EDITABLE: [&str; 4] = ["agents", "egress", "purge", "schedule"];
/// How long the data directory's size is reused before it's measured
/// again (it walks the directory).
const SIZE_TTL: Duration = Duration::from_secs(30);

/// The data directory's last measured size, and when it was measured.
type SizeCache = Arc<Mutex<Option<(Instant, Option<u64>)>>>;

pub struct Ops {
    pub control: Control,
    /// The registry's crawler section, applied by `sync_crawler`.
    crawler: tokio::sync::watch::Receiver<Crawler>,
    pub monitor: Monitor,
    pub search: Option<SearchState>,
    pub data_dir: PathBuf,
    size: SizeCache,
}

impl std::fmt::Debug for Ops {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ops")
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

impl Ops {
    pub fn new(
        control: Control,
        crawler: tokio::sync::watch::Receiver<Crawler>,
        monitor: Monitor,
        search: Option<SearchState>,
        data_dir: PathBuf,
    ) -> Ops {
        Ops {
            control,
            crawler,
            monitor,
            search,
            data_dir,
            size: Arc::default(),
        }
    }

    /// The data directory's size, measured at most every [`SIZE_TTL`]
    /// (blocking: it walks the directory).
    fn data_bytes(&self) -> impl FnOnce() -> Option<u64> + Send + 'static {
        let (size, dir) = (self.size.clone(), self.data_dir.clone());
        move || {
            let mut cached = size.lock().expect("size lock");
            if let Some((at, bytes)) = *cached
                && at.elapsed() < SIZE_TTL
            {
                return bytes;
            }
            let bytes = dir_size(&dir);
            *cached = Some((Instant::now(), bytes));
            bytes
        }
    }

    /// Everything but `data_bytes` and `index_lag_s`, which
    /// [`OpsApi::stats`] reads off the async runtime.
    fn stats_now(&self) -> StatsResponse {
        let now = crate::unix_now();
        let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
        let (harvested, fetched) = self.control.rates();
        StatsResponse {
            data_bytes: None,
            // std can't ask for a volume's free space; the crate that can
            // (fs4) is a new dependency, which needs the owner's approval.
            disk_free_bytes: None,
            fetched_per_hour: fetched,
            harvested_per_hour: harvested,
            index_lag_s: None,
            paused: self.control.status().paused,
            queue_depth: get(&self.monitor.pool_stats().pending),
            searchable_torrents: self.search.as_ref().map(|s| s.index.index().num_docs()),
            stored_torrents: self.monitor.torrents(),
            updated_at: now,
            uptime_s: now.saturating_sub(self.monitor.started_at()).max(0) as u64,
        }
    }
}

/// How far the index trails the store: 0 when everything stored is
/// searchable (`watermark` ≥ `max_id`); while it's behind, the seconds
/// since the index last committed, so a stalled index shows a growing
/// lag. `None` when it's behind and hasn't committed since the start
/// (say, a backlog being indexed after a restart).
fn index_lag(now: u64, max_id: i64, watermark: i64, index_commit: u64) -> Option<u64> {
    if watermark >= max_id {
        Some(0)
    } else if index_commit == 0 {
        None
    } else {
        Some(now.saturating_sub(index_commit))
    }
}

/// Bytes in the files under `dir`, not following links; `None` if it
/// can't be read.
pub(crate) fn dir_size(dir: &Path) -> Option<u64> {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).ok()? {
            let Ok(entry) = entry else { continue };
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    Some(total)
}

fn consent(c: Consent) -> ConsentClass {
    match c {
        Consent::None => ConsentClass::None,
        Consent::External => ConsentClass::External,
        Consent::Account => ConsentClass::Account,
        Consent::Restricted => ConsentClass::Restricted,
        Consent::Overlay => ConsentClass::Overlay,
    }
}

fn consent_back(c: ConsentClass) -> Consent {
    match c {
        ConsentClass::None => Consent::None,
        ConsentClass::External => Consent::External,
        ConsentClass::Account => Consent::Account,
        ConsentClass::Restricted => Consent::Restricted,
        ConsentClass::Overlay => Consent::Overlay,
    }
}

fn priority(p: Priority) -> SourcePriority {
    match p {
        Priority::Low => SourcePriority::Low,
        Priority::Normal => SourcePriority::Normal,
        Priority::High => SourcePriority::High,
        Priority::Max => SourcePriority::Max,
    }
}

fn priority_back(p: SourcePriority) -> Priority {
    match p {
        SourcePriority::Low => Priority::Low,
        SourcePriority::Normal => Priority::Normal,
        SourcePriority::High => Priority::High,
        SourcePriority::Max => Priority::Max,
    }
}

fn source(v: SourceView) -> Source {
    let m = &v.status.manifest;
    let state = match v.state {
        lodestar_store::SourceState::Enabled => SourceState::Enabled,
        lodestar_store::SourceState::Paused => SourceState::Paused,
        lodestar_store::SourceState::Disabled => SourceState::Disabled,
        lodestar_store::SourceState::Purged => SourceState::Purged,
    };
    let enabled = state == SourceState::Enabled;
    Source {
        consent: consent(m.consent),
        consent_required: control::needs_consent(m.consent),
        editable: EDITABLE.map(String::from).to_vec(),
        enabled,
        health: SourceHealth {
            harvested_24h: v.harvested_24h,
            last_error: v.status.last_error.clone(),
            last_run: v.last_run,
            running: v.status.running,
            stored_24h: v.stored_24h,
        },
        id: v.status.id.get(),
        kinds: m
            .kinds
            .iter()
            .map(|k| match k {
                Kind::Discovery => SourceKind::Discovery,
                Kind::Importer => SourceKind::Importer,
                Kind::PeerFinder => SourceKind::PeerFinder,
                Kind::Enrichment => SourceKind::Enrichment,
                Kind::Network => SourceKind::Network,
                Kind::Output => SourceKind::Output,
            })
            .collect(),
        name: m.name.to_string(),
        not_yet_editable: NOT_YET_EDITABLE.map(String::from).to_vec(),
        plugin: m.id.to_string(),
        priority: priority(v.priority),
        restart_required: enabled != v.status.enabled,
        risk: match m.risk {
            Risk::Low => RiskLevel::Low,
            Risk::Medium => RiskLevel::Medium,
            Risk::High => RiskLevel::High,
        },
        state,
        version: v.version,
    }
}

fn internal(e: impl std::fmt::Display) -> OpsError {
    OpsError::Internal(e.to_string())
}

impl OpsApi for Ops {
    fn crawler(&self) -> CrawlerRuntime {
        let s = self.control.status();
        CrawlerRuntime {
            since: s.since,
            dht: s.dht.map(|d| DhtStatus {
                bootstrapped: d.bootstrapped,
                outstanding: d.outstanding,
                routing_nodes: d.routing_nodes,
            }),
            fetch: FetchStatus {
                in_flight: s.fetching,
                lookups: s.lookups,
                queued: s.queued,
            },
        }
    }

    fn sync_crawler(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.control
                .sync(|| {
                    let c = self.crawler.borrow();
                    (c.paused, c.intensity.into())
                })
                .await;
        })
    }

    fn stats(&self) -> BoxFuture<'_, StatsResponse> {
        Box::pin(async move {
            let data_bytes = tokio::task::spawn_blocking(self.data_bytes())
                .await
                .ok()
                .flatten();
            let index_lag_s = match &self.search {
                Some(s) => {
                    let control = self.control.clone();
                    let max_id = tokio::task::spawn_blocking(move || control.max_torrent_id())
                        .await
                        .ok()
                        .and_then(Result::ok);
                    max_id.and_then(|max_id| {
                        index_lag(
                            crate::unix_now().max(0) as u64,
                            max_id,
                            s.stats.watermark.load(Ordering::Relaxed),
                            s.stats.last_commit.load(Ordering::Relaxed),
                        )
                    })
                }
                None => None,
            };
            StatsResponse {
                data_bytes,
                index_lag_s,
                ..self.stats_now()
            }
        })
    }

    fn sources(&self) -> BoxFuture<'_, Result<Vec<Source>, OpsError>> {
        Box::pin(async move {
            let control = self.control.clone();
            let views = tokio::task::spawn_blocking(move || control.sources())
                .await
                .map_err(internal)?
                .map_err(internal)?;
            Ok(views.into_iter().map(source).collect())
        })
    }

    fn patch_source(&self, id: u8, patch: SourcePatch) -> BoxFuture<'_, Result<Source, OpsError>> {
        Box::pin(async move {
            let id = SourceId::new(id).ok_or(OpsError::UnknownSource)?;
            let change = SourceChange {
                enabled: patch.enabled,
                priority: patch.priority.map(priority_back),
                consent: patch.consent.map(consent_back),
            };
            match self.control.update_source(id, change).await {
                Ok(v) => Ok(source(v)),
                Err(SourceError::Unknown) => Err(OpsError::UnknownSource),
                Err(SourceError::ConsentRequired(c)) => Err(OpsError::ConsentRequired(consent(c))),
                Err(SourceError::Store(e)) => Err(internal(e)),
            }
        })
    }

    fn network(&self) -> NetworkCheck {
        network(self.control.network())
    }

    fn restart_network_check(&self) -> Result<NetworkCheck, OpsError> {
        self.control
            .restart_network_check()
            .map(network)
            .map_err(|retry_after_secs| OpsError::RateLimited { retry_after_secs })
    }
}

fn network(n: control::NetworkCheck) -> NetworkCheck {
    let r = n.reach;
    NetworkCheck {
        checked_at: crate::unix_now(),
        external: ExternalAddress {
            agreeing: n.external.agreeing as u32,
            ip: n.external.ip.map(|ip| ip.to_string()),
            port: n.external.port,
            voters: n.external.voters as u32,
        },
        inbound_queries: r.inbound,
        mapping: PortMapping::Unavailable,
        measured_s: r.measured_secs,
        min_window_s: REACH_MIN_SECS,
        reachable: r.reachable,
        reason: match r.reason {
            control::ReachReason::DhtOff => ReachReason::DhtOff,
            control::ReachReason::Bootstrapping => ReachReason::Bootstrapping,
            control::ReachReason::Measuring => ReachReason::Measuring,
            control::ReachReason::UnsolicitedQueries => ReachReason::UnsolicitedQueries,
            control::ReachReason::FewUnsolicitedSources => ReachReason::FewUnsolicitedSources,
            control::ReachReason::NoUnsolicitedQueries => ReachReason::NoUnsolicitedQueries,
        },
        unsolicited_queries: r.unsolicited,
        unsolicited_sources: r.sources,
        window_s: REACH_WINDOW_SECS,
        window_started_at: n.window_started_at,
    }
}

/// Where the first-run log tells you to open the UI: the bind address if
/// it names one, else "this host's address" (a wildcard bind isn't an
/// address a browser can open).
pub fn ui_address(bind: std::net::SocketAddr) -> String {
    if bind.ip().is_unspecified() {
        format!("http://<this host's address>:{}/", bind.port())
    } else {
        format!("http://{bind}/")
    }
}

#[cfg(test)]
mod tests;
