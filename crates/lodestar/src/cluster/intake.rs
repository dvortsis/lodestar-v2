//! The pipeline's intake for remote workers, wired into the cluster port
//! (architecture §19.3–§19.5; workers plan WK-9).
//!
//! `lodestar_pipeline::remote` does the work: results under fencing, late
//! results, counters into the rollups with a worker dimension, peer hints
//! and backpressure. This module hands it what only `serve` has when the
//! port opens: the store check, each worker's clock offset (from the
//! server it's about to be given to), and the workers' standing (quarantine
//! and purge, which WK-13 adds; until then none is refused).

use std::sync::{Arc, OnceLock, Weak};

use lodestar_cluster::{ClusterServer, StoredKeys, WorkerId};
use lodestar_pipeline::remote::{Clocks, NoStanding, RemoteIntake, RemoteSink, SinkParts};

/// Each worker's offset as the server measured it from its heartbeats.
/// Before the server exists, or for a worker it doesn't know, 0.
#[derive(Debug, Default)]
struct ServerClocks {
    server: OnceLock<Weak<ClusterServer>>,
}

impl Clocks for ServerClocks {
    fn offset_ms(&self, worker: WorkerId) -> i64 {
        let Some(server) = self.server.get().and_then(Weak::upgrade) else {
            return 0;
        };
        server
            .snapshot()
            .workers
            .iter()
            .find(|w| w.worker == worker)
            .map_or(0, |w| w.offset_ms)
    }
}

/// The intake attached to one server.
pub(crate) struct Attached {
    pub sink: Arc<RemoteSink>,
    clocks: Arc<ServerClocks>,
    hints_allow_private: bool,
}

impl std::fmt::Debug for Attached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attached").finish_non_exhaustive()
    }
}

/// The sink and admission hook for the server about to be built.
/// `hints_allow_private` is the server's own (`ServerConfig`), so hints
/// pass the same network policy whichever way they're relayed.
pub(crate) fn attach(
    remote: &Arc<RemoteIntake>,
    stored: Arc<dyn StoredKeys>,
    hints_allow_private: bool,
) -> Attached {
    let clocks = Arc::new(ServerClocks::default());
    let sink = remote.sink(SinkParts {
        standing: Arc::new(NoStanding),
        stored,
        clocks: clocks.clone(),
        hints_allow_private,
    });
    Attached {
        sink,
        clocks,
        hints_allow_private,
    }
}

impl Attached {
    /// The server is built: offsets come from it, and the built-in
    /// worker's hints for keys remote workers hold go through it.
    pub fn serving(&self, remote: &RemoteIntake, server: &Arc<ClusterServer>) {
        let _ = self.clocks.server.set(Arc::downgrade(server));
        remote.serving(Some(server), self.hints_allow_private);
    }
}
