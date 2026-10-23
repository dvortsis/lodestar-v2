//! The binary's [`LogSource`]: `/logs` over the runtime's ring buffer and
//! log folder. `Services::run` layers it as an `Extension` and closes it
//! at shutdown.

use std::sync::Arc;

use lodestar_api::logs::{
    HistoryQuery, LineFilter, LogHistory, LogPurge, LogSource, LogStreamConfig, LogsError,
    RingRead, Transport,
};
use tokio::sync::watch;

use super::Runtime;
use super::files::{self, Folder};
use super::view::{data_kind, matches, ring_line};

/// `/logs` over a runtime.
pub struct Logs {
    rt: Option<&'static Runtime>,
    transport: Transport,
}

impl std::fmt::Debug for Logs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Logs").finish_non_exhaustive()
    }
}

/// The process's `/logs` source, over the installed runtime (none: every
/// route but the catalogue answers `503`).
pub fn source() -> Arc<dyn LogSource> {
    Arc::new(Logs::new(super::runtime(), LogStreamConfig::default()))
}

impl Logs {
    pub fn new(rt: Option<&'static Runtime>, config: LogStreamConfig) -> Logs {
        Logs {
            rt,
            transport: Transport::new(config),
        }
    }

    fn rt(&self) -> Result<&'static Runtime, LogsError> {
        self.rt.ok_or(LogsError::Unavailable)
    }

    fn folder(&self) -> Result<Folder, &'static str> {
        let Some(rt) = self.rt else {
            return Err("not_opened");
        };
        files::folder(rt.opened_folder(), &rt.configured_folder())
    }
}

impl LogSource for Logs {
    fn transport(&self) -> &Transport {
        &self.transport
    }

    fn epoch(&self) -> &str {
        self.rt.map_or("0000000000000000", |rt| rt.epoch())
    }

    fn bounds(&self) -> (u64, u64) {
        self.rt.map_or((0, 1), |rt| rt.ring().bounds())
    }

    fn listen(&self) -> (Box<dyn Send + Sync>, watch::Receiver<u64>) {
        match self.rt {
            Some(rt) => {
                let (guard, rx) = rt.ring().listen();
                (Box::new(guard), rx)
            }
            None => (Box::new(()), watch::channel(0).1),
        }
    }

    fn after(&self, cursor: u64, filter: &LineFilter, max: usize) -> RingRead {
        let Some(rt) = self.rt else {
            return RingRead::default();
        };
        let mask = rt.mask_now();
        let epoch = rt.epoch();
        let read = rt.ring().after(cursor, max, |e| {
            ring_line(e, mask, epoch).filter(|l| matches(l, filter))
        });
        RingRead {
            lines: read.lines,
            matching: read.matching,
            evicted: read.evicted,
            head: read.head,
        }
    }

    fn generation(&self) -> u64 {
        self.rt.map_or(0, |rt| rt.ring().generation())
    }

    fn purges_since(&self, generation: u64) -> (u64, Vec<LogPurge>) {
        let Some(rt) = self.rt else {
            return (0, Vec::new());
        };
        let (g, purges) = rt.ring().purges_since(generation);
        let out = purges
            .into_iter()
            .map(|p| LogPurge {
                kinds: p.kinds.kinds().map(data_kind).collect(),
                through: rt.id(p.through),
            })
            .collect();
        (g, out)
    }

    fn purges_covering(&self, seq: u64) -> Vec<LogPurge> {
        let Some(rt) = self.rt else {
            return Vec::new();
        };
        rt.ring()
            .purges_covering(seq)
            .into_iter()
            .map(|p| LogPurge {
                kinds: p.kinds.kinds().map(data_kind).collect(),
                through: rt.id(p.through),
            })
            .collect()
    }

    fn history(&self, query: &HistoryQuery) -> Result<LogHistory, LogsError> {
        let rt = self.rt()?;
        let folder = self.folder().ok();
        super::history::page(rt, folder.as_ref().map(|f| f.dir.as_path()), query)
    }
}
