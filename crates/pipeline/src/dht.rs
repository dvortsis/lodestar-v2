//! The Mainline DHT as source plugin #1 (architecture §5, §18.1): it
//! discovers keys (BEP 51 sampling, passive capture) and finds peers for
//! keys the fetch pool asks about (`get_peers` lookups).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_dht::node::{
    Config, Harvest, LookupEnd, Node, NodeHandle, SessionAdmit, SessionEnd, Source, StatsSnapshot,
};
#[cfg(test)]
use lodestar_metainfo::InfoHash;
use lodestar_network_policy::iface::{self, Down, State};
use lodestar_sources::{
    BoxFuture, Consent, Endpoint, Found, Kind, LookupSession, Manifest, Network, PeerRequest,
    PluginError, Priority, Risk, SessionAdmission, SourceCtx, SourcePlugin, Terminal,
};
use tokio::sync::{mpsc, oneshot, watch};

/// The DHT's harvest strategy codes, for per-strategy metrics. Persisted
/// in the rollups: append only.
pub mod strategy {
    pub const SAMPLE: u8 = 0;
    pub const GET_PEERS: u8 = 1;
    pub const ANNOUNCE: u8 = 2;
    pub const LOOKUP: u8 = 3;
    /// Not a DHT strategy: an incomplete import queued for its info dict.
    pub const REFETCH: u8 = 200;
}

pub(crate) static MANIFEST: Manifest = Manifest {
    id: "dht",
    name: "Mainline DHT",
    version: env!("CARGO_PKG_VERSION"),
    kinds: &[Kind::Discovery, Kind::PeerFinder],
    networks: &[Network::Bt],
    endpoints: &[
        Endpoint {
            protocol: "udp",
            host: "Mainline DHT nodes (any)",
        },
        Endpoint {
            protocol: "tcp",
            host: "BitTorrent peers (metadata only)",
        },
    ],
    risk: Risk::Low,
    consent: Consent::None,
    default_priority: Priority::Normal,
    enabled_by_default: true,
    always_adult: false,
    finds_private: false,
};

const PIN_RETRY_FIRST: Duration = Duration::from_secs(2);
const PIN_RETRY_MAX: Duration = Duration::from_secs(60);

pub struct DhtSource {
    config: Config,
    stats: Arc<Mutex<Option<StatsSnapshot>>>,
    node: Arc<Mutex<Option<NodeHandle>>>,
    /// The crawler's pause, handed to the node whenever it runs.
    paused: watch::Sender<bool>,
}

impl DhtSource {
    /// `config.paused` is the pause it starts with.
    pub fn new(config: Config) -> DhtSource {
        DhtSource {
            paused: watch::Sender::new(config.paused),
            config,
            stats: Arc::default(),
            node: Arc::default(),
        }
    }

    /// Pauses or resumes the node's harvesting (`NodeHandle::set_paused`),
    /// now if it runs and from its start if it doesn't yet.
    pub fn set_paused(&self, paused: bool) {
        self.paused.send_replace(paused);
    }

    /// The crawler's pause as last handed to the node.
    pub fn is_paused(&self) -> bool {
        *self.paused.borrow()
    }

    /// The running node, for BEP 33 scrapes: set once it has started and
    /// cleared when it stops. Clone the handle out rather than holding the
    /// lock across a scrape.
    pub fn node_handle(&self) -> Arc<Mutex<Option<NodeHandle>>> {
        self.node.clone()
    }

    /// The node's counters, refreshed every second while it runs.
    pub fn stats(&self) -> Option<StatsSnapshot> {
        *self.stats.lock().expect("dht stats lock")
    }

    /// Shared handle to the counters, for the metrics endpoint.
    pub fn stats_handle(&self) -> Arc<Mutex<Option<StatsSnapshot>>> {
        self.stats.clone()
    }
}

impl std::fmt::Debug for DhtSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DhtSource")
            .field("bind", &self.config.bind)
            .finish_non_exhaustive()
    }
}

fn found(h: Harvest) -> Found {
    let strategy = match h.source {
        Source::Sample => strategy::SAMPLE,
        Source::GetPeers => strategy::GET_PEERS,
        Source::Announce => strategy::ANNOUNCE,
        Source::Lookup => strategy::LOOKUP,
    };
    let mut found = Found::key(h.info_hash, strategy).with_peers(h.peers);
    // CR-HINT-FIRST: the pool trusts it only from this plugin's source,
    // and only with `dht.hint_first` on.
    found.hint = h.hint;
    found
}

/// The pool's streaming admission as the DHT actor sees it (CR-1a-S): the
/// two crates don't depend on each other, so the plugin adapts the session.
/// Allocates the terminal only when the actor admits the request.
struct PoolSession(SessionAdmission);

impl SessionAdmit for PoolSession {
    fn is_closed(&self) -> bool {
        self.0.is_closed()
    }

    fn admit(
        self: Box<Self>,
        batches: mpsc::Receiver<Vec<std::net::SocketAddr>>,
    ) -> Option<Box<dyn SessionEnd>> {
        let (tx, terminal) = oneshot::channel();
        self.0
            .send(Some(LookupSession { batches, terminal }))
            .ok()
            .map(|()| Box::new(PoolEnd(tx)) as Box<dyn SessionEnd>)
    }

    fn refuse(self: Box<Self>) {
        let _ = self.0.send(None);
    }
}

struct PoolEnd(oneshot::Sender<Terminal>);

impl SessionEnd for PoolEnd {
    fn end(self: Box<Self>, end: LookupEnd) {
        let _ = self.0.send(match end {
            LookupEnd::Complete => Terminal::Complete,
            LookupEnd::Deadline => Terminal::Deadline,
            LookupEnd::Cancelled => Terminal::Cancelled,
        });
    }
}

/// Hands one pool request to the node: a streaming one as a session, the
/// rest as before.
fn forward(node: &NodeHandle, request: PeerRequest) {
    let PeerRequest {
        key,
        admit,
        session,
        permit,
    } = request;
    match session {
        Some(session) => {
            // A streaming finder answers the session alone.
            drop(admit);
            node.request_session(key, Box::new(PoolSession(session)), permit);
        }
        None => {
            node.request_peers_with(key, admit, permit);
        }
    }
}

async fn next_request(
    rx: &mut Option<mpsc::Receiver<lodestar_sources::PeerRequest>>,
) -> Option<lodestar_sources::PeerRequest> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

impl SourcePlugin for DhtSource {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(async move {
            let (tx, mut harvests) = mpsc::channel(4096);
            let mut paused = self.paused.subscribe();
            let mut vpn = self.config.guard.as_ref().map(|g| g.subscribe());
            let mut requests_open = true;
            // Between pin failures (an old kernel without `CAP_NET_RAW`
            // fails every time): 2 s doubling to a minute.
            let mut retry = PIN_RETRY_FIRST;
            loop {
                // X-vpn-bind: no node, so no packet, until the interface
                // is up; each time it comes back, a node on fresh sockets.
                let generation = match vpn.as_mut() {
                    None => None,
                    Some(rx) => loop {
                        let state = *rx.borrow_and_update();
                        if let State::Up { generation } = state {
                            break Some(generation);
                        }
                        tokio::select! {
                            biased;
                            _ = ctx.shutdown.requested() => return Ok(()),
                            changed = rx.changed() => {
                                if changed.is_err() {
                                    // The guard is gone: stay down.
                                    ctx.shutdown.requested().await;
                                    return Ok(());
                                }
                            }
                            // Refused (dropped) while there's no node.
                            req = next_request(&mut ctx.peer_requests), if requests_open => {
                                requests_open = req.is_some();
                            }
                        }
                    },
                };
                let config = Config {
                    paused: *paused.borrow_and_update(),
                    ..self.config.clone()
                };
                let (node, generation) = match (
                    Node::start(config, Some(tx.clone())).await,
                    &self.config.guard,
                ) {
                    (Ok(node), _) => (node, generation),
                    // X-vpn-bind: the interface is there but the sockets
                    // can't be pinned to it (Linux before 5.7 without
                    // `CAP_NET_RAW`, or it went just now). Fail closed: the
                    // guard holds the crawl down, so the retries below don't
                    // flap it up and down, until a start works.
                    (Err(e), Some(guard)) if iface::is_refused(&e) => {
                        lodestar_log::log_event!(
                            crate::log_events::VPN_PIN_FAILED,
                            error = lodestar_log::Error::new(&e)
                        );
                        guard.hold(Down::BindFailed);
                        let started = loop {
                            tokio::select! {
                                biased;
                                _ = ctx.shutdown.requested() => {
                                    guard.release();
                                    return Ok(());
                                }
                                () = tokio::time::sleep(retry) => {}
                            }
                            retry = (retry * 2).min(PIN_RETRY_MAX);
                            if !guard.link_up() {
                                // Gone meanwhile: wait for it as usual.
                                break None;
                            }
                            // Read before the start: a link that changes
                            // while it binds makes this node stale at once.
                            let generation = guard.generation();
                            let config = Config {
                                paused: *paused.borrow_and_update(),
                                ..self.config.clone()
                            };
                            match Node::start(config, Some(tx.clone())).await {
                                Ok(node) => break Some((node, generation)),
                                Err(e) if iface::is_refused(&e) => {}
                                Err(e) => {
                                    guard.release();
                                    return Err(PluginError(format!("DHT bind failed: {e}")));
                                }
                            }
                        };
                        guard.release();
                        let Some((node, generation)) = started else {
                            continue;
                        };
                        (node, Some(generation))
                    }
                    (Err(e), _) => return Err(PluginError(format!("DHT bind failed: {e}"))),
                };
                lodestar_log::log_event!(
                    crate::log_events::DHT_STARTED,
                    sockets = node.local_addrs().len()
                );
                retry = PIN_RETRY_FIRST;
                *self.node.lock().expect("dht handle lock") = Some(node.handle());
                let mut tick = tokio::time::interval(Duration::from_secs(1));
                let stopping = loop {
                    tokio::select! {
                        biased;
                        _ = ctx.shutdown.requested() => break true,
                        () = left(&mut vpn, generation) => break false,
                        Ok(()) = paused.changed() => {
                            let p = *paused.borrow_and_update();
                            node.handle().set_paused(p).await;
                        }
                        req = next_request(&mut ctx.peer_requests), if requests_open => match req {
                            Some(request) => forward(&node.handle(), request),
                            None => requests_open = false,
                        },
                        _ = tick.tick() => {
                            *self.stats.lock().expect("dht stats lock") = Some(node.stats());
                        }
                        Some(h) = harvests.recv() => {
                            // Lookups answer the pool's requests; the rest is
                            // discovery.
                            match (&ctx.peer_replies, h.source) {
                                (Some(replies), Source::Lookup) => replies.try_emit(found(h)),
                                _ => ctx.emitter.try_emit(found(h)),
                            };
                        }
                    }
                };
                *self.node.lock().expect("dht handle lock") = None;
                let saved = node
                    .shutdown()
                    .await
                    .map_err(|e| PluginError(format!("saving DHT state failed: {e}")));
                if stopping {
                    return saved;
                }
                // The interface went: the next node starts from the
                // state just saved, on sockets bound once it's back.
                if let Err(e) = saved {
                    lodestar_log::log_event!(
                        crate::log_events::SOURCE_FAILED,
                        plugin = MANIFEST.id,
                        error = lodestar_log::Error::new(&e)
                    );
                }
            }
        })
    }
}

/// Returns once the guard's state is no longer `Up` under `generation`;
/// never without a guard.
async fn left(vpn: &mut Option<watch::Receiver<State>>, generation: Option<u64>) {
    let (Some(rx), Some(generation)) = (vpn.as_mut(), generation) else {
        return std::future::pending().await;
    };
    loop {
        if *rx.borrow_and_update() != (State::Up { generation }) {
            return;
        }
        if rx.changed().await.is_err() {
            // The guard is gone: nothing can change any more.
            return std::future::pending().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_adapter_allocates_on_admission_and_maps_every_end() {
        for (end, want) in [
            (LookupEnd::Complete, Terminal::Complete),
            (LookupEnd::Deadline, Terminal::Deadline),
            (LookupEnd::Cancelled, Terminal::Cancelled),
        ] {
            let (tx, mut rx) = oneshot::channel();
            let admit: Box<dyn SessionAdmit> = Box::new(PoolSession(tx));
            assert!(!admit.is_closed());
            let (btx, brx) = mpsc::channel(2);
            let sender = admit.admit(brx).expect("the pool is waiting");
            let mut session = rx.try_recv().unwrap().expect("a session");
            btx.try_send(vec!["10.0.0.1:1".parse().unwrap()]).unwrap();
            drop(btx);
            sender.end(end);
            assert_eq!(session.terminal.try_recv(), Ok(want));
            assert_eq!(session.batches.try_recv().unwrap().len(), 1);
        }
        // Refused: the pool reads a refusal, not a closed admission.
        let (tx, mut rx) = oneshot::channel();
        Box::new(PoolSession(tx)).refuse();
        assert!(matches!(rx.try_recv(), Ok(None)));
        // A gone pool: nothing is allocated for it.
        let (tx, rx) = oneshot::channel();
        drop(rx);
        let admit: Box<dyn SessionAdmit> = Box::new(PoolSession(tx));
        assert!(admit.is_closed());
        assert!(admit.admit(mpsc::channel(2).1).is_none());
    }

    #[test]
    fn harvests_map_to_strategies() {
        let h = |source| Harvest {
            info_hash: InfoHash([1; 20]),
            source,
            peers: vec!["10.0.0.1:1".parse().unwrap()],
            hint: false,
        };
        assert_eq!(found(h(Source::Sample)).strategy, strategy::SAMPLE);
        assert_eq!(found(h(Source::GetPeers)).strategy, strategy::GET_PEERS);
        assert_eq!(found(h(Source::Announce)).strategy, strategy::ANNOUNCE);
        let f = found(h(Source::Lookup));
        assert_eq!((f.strategy, f.peers.len()), (strategy::LOOKUP, 1));
        assert!(
            !f.private_source,
            "the public DHT is never a private source"
        );
        assert!(!f.hint);
        let hinted = Harvest {
            hint: true,
            ..h(Source::Sample)
        };
        assert!(found(hinted).hint, "CR-HINT-FIRST's mark reaches the pool");
    }
}
