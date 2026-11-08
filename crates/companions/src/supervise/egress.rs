//! The egress gate (plan §4 CMP-4b; §5 Q8, Q9, Q18; architecture §18.4).
//!
//! Each companion follows its own route (Q18: the `companions.<id>.egress`
//! setting, CMP-5's). `crates/lodestar` builds it through
//! `lodestar_sources::EgressFactory`, as for a source plugin, and hands the
//! [`PluginEgress`] in; this crate never depends on `crates/pipeline`.
//!
//! - **Can it follow the route?** [`decide`] says, from what the companion
//!   supports ([`Follows`]): qBittorrent binds an interface or an address;
//!   Prowlarr uses a SOCKS5 proxy. A route it can't follow means it never
//!   starts ([`GateError::CantFollow`]: "Can't follow your VPN setting").
//!   It's never started on the direct route instead (Q8). Configuring the
//!   companion's own bind or proxy setting is CMP-5's, from the decision.
//! - **Up or down** (Q9): a companion whose route isn't direct starts only
//!   while the route is up. When the probe reports it down, the companion
//!   is held (stopped, through the supervisor's [`Handle::hold`]); when the
//!   route is back it's released and starts again. A hold isn't a crash,
//!   so route flaps never reach "Needs attention". A route not checked yet
//!   counts as down, and so does a route whose state can't be read any
//!   more.
//!
//! The probe itself runs where the route is built: whoever builds the
//! [`PluginEgress`] also runs its [`PluginEgress::monitor`] (as the
//! pipeline does for plugins), so its state changes.

use lodestar_sources::egress::{PluginEgress, RouteKind, RouteState};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::{Companion, Handle, SuperviseError, Supervisor};
use crate::inputs::CompanionId;

/// The route kinds a companion can follow besides direct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Follows {
    pub bind_address: bool,
    pub interface: bool,
    pub socks5: bool,
}

impl Follows {
    /// Nothing but direct.
    pub const NONE: Follows = Follows {
        bind_address: false,
        interface: false,
        socks5: false,
    };

    /// What the catalogue's companions support: qBittorrent binds an
    /// interface or an address (`Connection\Interface`,
    /// `Connection\InterfaceAddress`); Prowlarr sends through a SOCKS5
    /// proxy. Any other companion follows only direct until its adapter
    /// says otherwise.
    pub fn of(id: &CompanionId) -> Follows {
        match id.as_str() {
            "qbittorrent" => Follows {
                bind_address: true,
                interface: true,
                socks5: false,
            },
            "prowlarr" => Follows {
                socks5: true,
                ..Follows::NONE
            },
            _ => Follows::NONE,
        }
    }

    pub fn can(&self, kind: RouteKind) -> bool {
        match kind {
            RouteKind::Direct => true,
            RouteKind::BindAddress => self.bind_address,
            RouteKind::Interface => self.interface,
            RouteKind::Socks5 => self.socks5,
        }
    }
}

/// Whether and how a companion follows its route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Direct: it runs whenever it's supervised.
    Direct,
    /// It follows this route kind (CMP-5 configures its bind or proxy
    /// setting for it) and runs only while the route is up.
    Follow(RouteKind),
}

/// Why a companion won't be started on its route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GateError {
    /// It can't follow this route kind: "Can't follow your VPN setting".
    CantFollow(RouteKind),
    /// The supervisor refused it.
    Supervise(SuperviseError),
}

impl GateError {
    pub fn code(&self) -> &'static str {
        match self {
            GateError::CantFollow(_) => "cant_follow_route",
            GateError::Supervise(e) => e.code(),
        }
    }
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "companion route: {}", self.code())
    }
}

impl std::error::Error for GateError {}

/// What a companion of `follows` does on a route of `kind`.
pub fn decide(follows: Follows, kind: RouteKind) -> Result<Decision, GateError> {
    match kind {
        RouteKind::Direct => Ok(Decision::Direct),
        k if follows.can(k) => Ok(Decision::Follow(k)),
        k => Err(GateError::CantFollow(k)),
    }
}

/// A companion's route: its kind and its live state.
#[derive(Debug, Clone)]
pub struct Route {
    pub kind: RouteKind,
    pub state: watch::Receiver<RouteState>,
}

impl Route {
    /// The route of a built [`PluginEgress`].
    pub fn of(egress: &PluginEgress) -> Route {
        Route {
            kind: egress.status().route,
            state: egress.watch(),
        }
    }
}

/// A gated companion. Dropping it stops following the route; the
/// companion stays as it is (held or not) until the supervisor removes it.
#[derive(Debug)]
pub struct Gated {
    pub decision: Decision,
    task: Option<JoinHandle<()>>,
}

impl Drop for Gated {
    fn drop(&mut self) {
        if let Some(t) = &self.task {
            t.abort();
        }
    }
}

/// Adds `c` to `sup` behind its route. A route it can't follow adds
/// nothing. On a non-direct route it's added held and released only while
/// the route is up.
pub fn add(
    sup: &mut Supervisor,
    c: Companion,
    follows: Follows,
    route: Route,
) -> Result<Gated, GateError> {
    let decision = decide(follows, route.kind)?;
    let id = c.spawn.id.clone();
    if decision == Decision::Direct {
        sup.add(c).map_err(GateError::Supervise)?;
        return Ok(Gated {
            decision,
            task: None,
        });
    }
    sup.add_held(c).map_err(GateError::Supervise)?;
    let handle = sup
        .handle(&id)
        .ok_or(GateError::Supervise(SuperviseError::Duplicate))?;
    Ok(Gated {
        decision,
        task: Some(tokio::spawn(follow(handle, route.state))),
    })
}

/// Holds the companion while the route isn't up, releases it while it is.
async fn follow(handle: Handle, mut state: watch::Receiver<RouteState>) {
    loop {
        let up = *state.borrow_and_update() == RouteState::Up;
        let sent = if up { handle.release() } else { handle.hold() };
        if !sent {
            return;
        }
        if state.changed().await.is_err() {
            // Nothing reports the route any more: fail closed.
            handle.hold();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> CompanionId {
        CompanionId::parse(s).unwrap()
    }

    #[test]
    fn each_companion_follows_what_it_supports() {
        let q = Follows::of(&id("qbittorrent"));
        let p = Follows::of(&id("prowlarr"));
        assert_eq!(
            decide(q, RouteKind::Interface),
            Ok(Decision::Follow(RouteKind::Interface))
        );
        assert_eq!(
            decide(q, RouteKind::BindAddress),
            Ok(Decision::Follow(RouteKind::BindAddress))
        );
        assert_eq!(
            decide(q, RouteKind::Socks5),
            Err(GateError::CantFollow(RouteKind::Socks5))
        );
        assert_eq!(
            decide(p, RouteKind::Socks5),
            Ok(Decision::Follow(RouteKind::Socks5))
        );
        for k in [RouteKind::Interface, RouteKind::BindAddress] {
            assert_eq!(decide(p, k), Err(GateError::CantFollow(k)));
        }
        let other = Follows::of(&id("gluetun"));
        assert_eq!(decide(other, RouteKind::Direct), Ok(Decision::Direct));
        assert!(decide(other, RouteKind::Socks5).is_err());
        assert_eq!(
            GateError::CantFollow(RouteKind::Socks5).code(),
            "cant_follow_route"
        );
    }
}
