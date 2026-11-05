//! Claimed trackers from feeds and plugins pass the shared host guard
//! before storage (X-claim-trackers): none on a literal LAN, loopback or
//! link-local address or a local name, and only in the form that was
//! judged.

use lodestar_pipeline::unverified::tracker_url;

#[test]
fn claimed_trackers_on_public_hosts_are_kept() {
    for ok in [
        "udp://tracker.example:1337/announce",
        "https://tracker.example/announce",
        "http://tracker.example:6969/announce",
        "https://t.example/a?passkey=x",
    ] {
        assert!(tracker_url(ok), "{ok}");
    }
}

#[test]
fn claimed_trackers_that_reach_the_users_network_are_dropped() {
    for bad in [
        "udp://192.168.1.1:80/announce",
        "http://127.0.0.1/",
        "http://127.1/announce",
        "http://0x7f.1/announce",
        "udp://[::ffff:127.0.0.1]:6969/announce",
        "http://[::127.0.0.1]/announce",
        "http://169.254.169.254/latest/meta-data/",
        "http://100.64.0.1/announce",
        "http://localhost/announce",
        "udp://LOCALHOST:6969/announce",
        "http://foo.localhost/announce",
        "http://ⓛocalhost/announce",
        "http://router/announce",
        "http://nas.local/announce",
        "https://user@t.example/announce",
        "wss://t.example/announce",
    ] {
        assert!(!tracker_url(bad), "{bad}");
    }
}

/// What's stored is what was judged: a spelling the `url` crate would
/// rewrite is dropped rather than stored as written, since another
/// parser could read its text as a different host.
#[test]
fn only_the_judged_form_is_stored() {
    for raw in [
        "HTTP://Tracker.Example/announce",
        "http://tracker.example",
        "http://t.example\\@127.0.0.1/announce",
        "http:1.1/announce",
    ] {
        assert!(!tracker_url(raw), "{raw}");
    }
}
