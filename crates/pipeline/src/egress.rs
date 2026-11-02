//! Each running plugin's egress (§18.4, §18.7; SRC-5's kill switch): the
//! client built on the source's egress setting, the monitor that probes a
//! non-direct route, and the warnings the start logs.

use std::sync::Arc;

use lodestar_sources::{
    DownReason, Egress, EgressFactory, Endpoints, Manifest, PluginEgress, RouteKind, RouteState,
    Shutdown,
};
use tokio::task::JoinHandle;

use crate::control::EgressSetting;
use crate::log_events;

/// The environment variables a user sets to proxy a program's HTTPS
/// traffic. Source plugins never use them (SRC-5): a proxy that resolves
/// names itself would bypass the SSRF guard.
pub const PROXY_ENV: [&str; 4] = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"];

/// Whether one of [`PROXY_ENV`] is set (to anything but empty).
pub fn proxy_env_set() -> bool {
    proxy_env_in(|k| std::env::var_os(k))
}

fn proxy_env_in(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    PROXY_ENV
        .iter()
        .any(|k| get(k).is_some_and(|v| !v.is_empty()))
}

/// Builds `m`'s client on `setting`: the manifest's endpoints, through
/// the route it names. Also returns the route it was built with (`None`
/// when a proxy's name didn't resolve; the client is then down for good).
pub async fn build(
    factory: &EgressFactory,
    m: &Manifest,
    setting: &EgressSetting,
) -> (Arc<PluginEgress>, Option<Egress>) {
    let endpoints = Endpoints::manifest(m.endpoints);
    match setting {
        EgressSetting::Direct => {
            let e = factory.build(m.id, &Egress::Direct, endpoints).await;
            (Arc::new(e), Some(Egress::Direct))
        }
        EgressSetting::Socks5(spec) => {
            let (e, route) = factory.build_socks5(m.id, spec, endpoints).await;
            (Arc::new(e), route)
        }
    }
}

/// `reason`'s label in the log and on `/metrics`: names no address, URL
/// or credential.
pub fn reason_label(reason: DownReason) -> &'static str {
    match reason {
        DownReason::Unsupported => "unsupported",
        DownReason::InvalidInterface => "invalid_interface",
        DownReason::InterfaceMissing => "interface_missing",
        DownReason::InterfaceDown => "interface_down",
        DownReason::AddressUnavailable => "address_unavailable",
        DownReason::ProxyUnreachable => "proxy_unreachable",
        DownReason::CredentialsUnavailable => "credentials_unavailable",
        DownReason::ClientUnavailable => "client_unavailable",
        DownReason::BindAddressUnsupported => "bind_address_unsupported",
        DownReason::ProxyUnresolved => "proxy_unresolved",
    }
}

/// What a route's new state means for the log, given whether it was last
/// logged as down: `Some(Some(reason))` went down, `Some(None)` came back,
/// `None` nothing to log. `Unchecked` is neither.
fn transition(down: &mut bool, state: RouteState) -> Option<Option<DownReason>> {
    match state {
        RouteState::Down(reason) if !*down => {
            *down = true;
            Some(Some(reason))
        }
        RouteState::Up if *down => {
            *down = false;
            Some(None)
        }
        _ => None,
    }
}

/// For a non-direct route: probes it every `every`, so a route that goes
/// down abandons the requests in flight, and logs each change between up
/// and down, until the plugins shut down. A direct route has nothing to
/// probe and gets no task (`None`).
pub fn watch(
    plugin: &'static str,
    egress: Arc<PluginEgress>,
    every: std::time::Duration,
    mut shutdown: Shutdown,
) -> Option<JoinHandle<()>> {
    if egress.status().route == RouteKind::Direct {
        return None;
    }
    Some(tokio::spawn(async move {
        let mut states = egress.watch();
        let mut down = false;
        let monitor = egress.monitor(every);
        tokio::pin!(monitor);
        loop {
            let state = *states.borrow_and_update();
            match transition(&mut down, state) {
                Some(Some(reason)) => lodestar_log::log_event!(
                    log_events::SOURCE_EGRESS_DOWN,
                    plugin = plugin,
                    reason = reason_label(reason)
                ),
                Some(None) => {
                    lodestar_log::log_event!(log_events::SOURCE_EGRESS_UP, plugin = plugin)
                }
                None => {}
            }
            tokio::select! {
                () = shutdown.requested() => break,
                () = &mut monitor => break,
                changed = states.changed() => if changed.is_err() { break },
            }
        }
    }))
}

/// Warns, once per start, for each running plugin that connects directly
/// while a proxy variable is set: the user may think it covers them.
pub fn warn_proxy_env(direct: &[&'static str]) {
    if !proxy_env_set() {
        return;
    }
    for plugin in direct {
        lodestar_log::log_event!(log_events::SOURCE_PROXY_ENV_IGNORED, plugin = *plugin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::time::Duration;

    #[test]
    fn transitions_log_once_per_change() {
        let mut down = false;
        assert_eq!(transition(&mut down, RouteState::Unchecked), None);
        assert_eq!(transition(&mut down, RouteState::Up), None);
        let gone = RouteState::Down(DownReason::ProxyUnreachable);
        assert_eq!(
            transition(&mut down, gone),
            Some(Some(DownReason::ProxyUnreachable))
        );
        assert_eq!(transition(&mut down, gone), None);
        assert_eq!(transition(&mut down, RouteState::Up), Some(None));
        assert_eq!(transition(&mut down, RouteState::Up), None);
    }

    #[test]
    fn proxy_variables_count_when_set_and_not_empty() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        assert!(!proxy_env_in(env(&[])));
        assert!(!proxy_env_in(env(&[("HTTPS_PROXY", "")])));
        assert!(!proxy_env_in(env(&[("HTTP_PROXY", "http://p:3128")])));
        for k in PROXY_ENV {
            let pairs: &'static [(&'static str, &'static str)] =
                Box::leak(Box::new([(k, "http://p:3128")]));
            assert!(proxy_env_in(env(pairs)), "{k}");
        }
    }

    fn manifest() -> &'static Manifest {
        static M: Manifest = Manifest {
            id: "feed",
            name: "Feed",
            version: "1",
            kinds: &[lodestar_sources::Kind::Discovery],
            networks: &[lodestar_sources::Network::Bt],
            endpoints: &[],
            risk: lodestar_sources::Risk::Low,
            consent: lodestar_sources::Consent::None,
            default_priority: lodestar_sources::Priority::Normal,
            enabled_by_default: false,
        };
        &M
    }

    /// The monitor finds a proxy that went away with no request sent, and
    /// sees it come back; it ends when the plugins shut down. A direct
    /// route gets no task.
    #[tokio::test]
    async fn the_monitor_follows_the_route_and_stops_at_shutdown() {
        let factory = EgressFactory::new(Arc::new(lodestar_sources::egress::NoCredentials));
        let (direct, _) = build(&factory, manifest(), &EgressSetting::Direct).await;
        let (_stop, shutdown) = Shutdown::new();
        assert!(watch("feed", direct, Duration::from_millis(20), shutdown).is_none());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let setting = EgressSetting::Socks5(lodestar_sources::ProxySpec {
            host: "127.0.0.1".into(),
            port: addr.port(),
            dns: None,
        });
        let (egress, _) = build(&factory, manifest(), &setting).await;
        let (stop, shutdown) = Shutdown::new();
        let task = watch(
            "feed",
            Arc::clone(&egress),
            Duration::from_millis(20),
            shutdown,
        )
        .unwrap();
        let states = egress.watch();
        let wait = |want: fn(&RouteState) -> bool| {
            let mut states = states.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(20), states.wait_for(want))
                    .await
                    .expect("the monitor didn't get there within the deadline")
                    .map(|s| *s)
                    .unwrap()
            }
        };
        assert_eq!(wait(|s| *s == RouteState::Up).await, RouteState::Up);
        drop(listener);
        assert_eq!(
            wait(|s| matches!(s, RouteState::Down(_))).await,
            RouteState::Down(DownReason::ProxyUnreachable)
        );
        let back = tokio::net::TcpListener::bind(addr).await;
        if back.is_ok() {
            assert_eq!(wait(|s| *s == RouteState::Up).await, RouteState::Up);
        }
        assert_eq!(egress.status().counters.answered, 0);
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .expect("the monitor didn't stop at shutdown")
            .unwrap();
    }
}
