//! Each running plugin's egress (§18.4, §18.7; SRC-5's kill switch): the
//! client built on the source's egress setting, the monitor that probes a
//! non-direct route, and the warnings the start logs.

use std::sync::Arc;

use lodestar_sources::{
    DownReason, Egress, EgressFactory, Endpoints, Manifest, PluginEgress, RouteState, Shutdown,
};

use crate::control::EgressSetting;
use crate::log_events;

/// The environment variables a user sets to proxy a program's HTTPS
/// traffic. Source plugins never use them (SRC-5): a proxy that resolves
/// names itself would bypass the SSRF guard.
pub const PROXY_ENV: [&str; 4] = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"];

/// Whether one of [`PROXY_ENV`] is set (to anything but empty).
pub fn proxy_env_set() -> bool {
    PROXY_ENV
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
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

/// Probes a non-direct route every [`crate::EGRESS_PROBE_EVERY`] and logs
/// each change between up and down, until the plugins shut down.
pub fn watch(plugin: &'static str, egress: Arc<PluginEgress>, mut shutdown: Shutdown) {
    tokio::spawn(async move {
        let mut states = egress.watch();
        let mut down = false;
        let direct = egress.status().route == lodestar_sources::RouteKind::Direct;
        let monitor = async {
            if direct {
                std::future::pending::<()>().await;
            }
            egress.monitor(crate::EGRESS_PROBE_EVERY).await;
        };
        tokio::pin!(monitor);
        loop {
            let state = *states.borrow_and_update();
            match state {
                RouteState::Down(reason) if !down => {
                    down = true;
                    lodestar_log::log_event!(
                        log_events::SOURCE_EGRESS_DOWN,
                        plugin = plugin,
                        reason = reason_label(reason)
                    );
                }
                RouteState::Up if down => {
                    down = false;
                    lodestar_log::log_event!(log_events::SOURCE_EGRESS_UP, plugin = plugin);
                }
                _ => {}
            }
            tokio::select! {
                () = shutdown.requested() => break,
                () = &mut monitor => break,
                changed = states.changed() => if changed.is_err() { break },
            }
        }
    });
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
