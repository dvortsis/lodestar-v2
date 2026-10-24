//! The filter crate's log events (architecture §12.1).

lodestar_log::catalogue! {
    "lodestar-filter";
    /// The subscribed IP lists block a large share of IPv4: the crawler
    /// sees correspondingly less of the DHT. `percent` is a whole number.
    FILTERS_IP_LISTS_BROAD {
        id: "filters.ip_lists_broad",
        component: Filters,
        level: Warn,
        message: "Subscribed IP lists block a large share of IPv4 addresses; the crawler will see correspondingly less of the DHT",
        fields: { percent: Size },
    }
}
