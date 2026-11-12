//! When the crawler first saw each hash, read in process from the DHT's
//! own `dht.key_harvested` event (off by default; switched on here).
//!
//! Every node in the process emits that event, but only the crawler
//! samples (the network's nodes have BEP 51 traversal off), and a sample
//! harvest is reported only the first time the node sees the hash. So
//! a hash's first `sample` event is the crawler's first sighting. The
//! peers are announced before the crawler starts, so nothing shows the
//! crawler a hash any earlier, passively.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use lodestar_log::Value;
use lodestar_log::{Record, Sink, set_sink};

/// Each hash's first BEP 51 sample harvest, by any node in the process.
static SAMPLED: Mutex<BTreeMap<[u8; 20], Instant>> = Mutex::new(BTreeMap::new());

struct Harvests;

impl Sink for Harvests {
    fn record(&self, record: &Record<'_>) {
        if record.event.id != lodestar_dht::log_events::KEY_HARVESTED::EVENT.id {
            return;
        }
        let mut hash = None;
        let mut sample = false;
        for (name, value) in record.fields {
            match (*name, value) {
                ("torrent", Value::Torrent(h)) => hash = Some(**h),
                ("source", Value::Label(l)) => sample = *l == "sample",
                _ => {}
            }
        }
        if let (Some(h), true) = (hash, sample) {
            let now = Instant::now();
            SAMPLED
                .lock()
                .expect("harvest log lock")
                .entry(h)
                .or_insert(now);
        }
    }
}

/// Installs the sink and switches the event on, once per process. The
/// sink drops every other event, which keeps the test quiet.
pub fn record() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        set_sink(&Harvests).expect("the only log sink in this test binary");
        lodestar_dht::log_events::KEY_HARVESTED::EVENT.set_on(true);
    });
}

/// When `hash` was first sampled, if it has been.
pub fn sampled(hash: &[u8; 20]) -> Option<Instant> {
    SAMPLED.lock().expect("harvest log lock").get(hash).copied()
}
