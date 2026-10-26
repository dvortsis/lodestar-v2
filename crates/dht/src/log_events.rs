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
    /// What the harvest found in the last `seconds`, by source, and the
    /// keys dropped because the pipeline was behind.
    HARVEST_SUMMARY {
        id: "dht.harvest_summary",
        component: Dht,
        level: Info,
        message: "DHT harvest in the last minute",
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
