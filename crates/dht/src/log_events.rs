//! The DHT's log events (architecture §12.1). A harvested key is a
//! `Torrent`; peers are counted, never named. At the harvest's peak rate
//! (35,000 keys a second from samples) a line per key would cost 40% of a
//! core and hundreds of GiB a day, so `dht.key_harvested` is `Trace` and a
//! summary every minute is on by default instead (B-log1b's `default_on`
//! rule).

lodestar_log::catalogue! {
    "lodestar-dht";
    /// Socket buffer requests and kernel readback, per bound socket. Zero
    /// grants with read errors mean unknown, not a zero-sized buffer.
    /// `send_requested=0` means the existing system default is retained.
    SOCKET_BUFFERS {
        id: "dht.socket_buffers",
        component: Dht,
        level: Info,
        message: "DHT socket buffers",
        fields: { receive_requested: Size, send_requested: Size, receive: Size, send: Size, set_errors: Size, read_errors: Size },
    }
    /// No configured bootstrap router could be queried or answered in a
    /// bootstrap round: `configured` names, `resolved` addresses, and of
    /// the last attempt's, `refused` by the outbound policy or the socket
    /// and `throttled` by the query budget. Written once until a router
    /// answers again. Fake-ip DNS and NAT64 answers are allowed; check DNS,
    /// the firewall and the VPN.
    BOOTSTRAP_UNREACHABLE {
        id: "dht.bootstrap_unreachable",
        component: Dht,
        level: Warn,
        message: "No DHT bootstrap router answered",
        fields: { configured: Size, resolved: Size, refused: Size, throttled: Size },
    }
    /// The restart proof's start check (CR-0d): whether this run resumes
    /// from the last clean stop (`mode`), and if not, why (`reason`, the
    /// `dht_sample_suppression_reason` label). `previous` is the marker the
    /// last run left (`clean`, `dirty`, `off`, `none` or `bad`; `unknown` on
    /// a filesystem without locks, where `mode` is `lockless`); `restored`
    /// counts the restrictions seeded, `dropped_expired` those already
    /// over. Counts only: no addresses.
    RESTART_CHECK {
        id: "dht.restart_check",
        component: Dht,
        level: Info,
        message: "DHT restart check",
        fields: { mode: Label, reason: Label, previous: Label, generation: Size, restored: Size, dropped_expired: Size },
    }
    /// The restart proof's commit at shutdown: `ok`, how long it took, and
    /// how many restrictions it wrote. Not ok means the next start
    /// suppresses for six hours.
    RESTART_COMMIT {
        id: "dht.restart_commit",
        component: Dht,
        level: Info,
        message: "DHT restart proof committed",
        fields: { ok: Label, duration_ms: Size, entries: Size },
    }
    /// A restart-proof write failed (`step`: lock, witness, invalidate,
    /// state_bin, snapshot or marker; `kind`: the error, such as
    /// `permission_denied`, `read_only` or `storage_full`). Sampling stays
    /// safe: the next start suppresses, or this run waits until the write
    /// lands.
    RESTART_PERSIST_FAILED {
        id: "dht.restart_persist_failed",
        component: Dht,
        level: Warn,
        message: "DHT restart proof write failed",
        fields: { step: Label, kind: Label },
    }
    /// A key reached the harvest: from samples, someone's `get_peers`, an
    /// announce, or a lookup this node ran (`source`).
    KEY_HARVESTED {
        id: "dht.key_harvested",
        component: Dht,
        level: Trace,
        default_on: false,
        message: "Key harvested",
        fields: { torrent: Hash, source: Label, peers: Size },
    }
    /// What the harvest found since the last summary (`seconds` ago, about
    /// a minute), by source, and the keys dropped because the pipeline was
    /// behind.
    HARVEST_SUMMARY {
        id: "dht.harvest_summary",
        component: Dht,
        level: Info,
        message: "DHT harvest since the last summary",
        fields: { seconds: Size, sample: Size, get_peers: Size, announce: Size, lookups: Size, dropped: Size },
    }
}

/// How often the summary is written.
pub const SUMMARY_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(test)]
mod tests {
    #[test]
    fn ids_are_unique_and_the_key_is_trace() {
        lodestar_log::check_ids(&[&super::CATALOGUE]).unwrap();
        let e = &super::KEY_HARVESTED::EVENT;
        assert_eq!(e.level, lodestar_log::Level::Trace);
        assert!(!e.default_on);
    }
}
