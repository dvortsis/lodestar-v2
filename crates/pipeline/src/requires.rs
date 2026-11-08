//! Plugins that need a companion (architecture §18.10; plan
//! 2026-11-06-v06-companions §4.3, CMP-10).
//!
//! `lodestar_companions::requires` says which plugins need what. Here the
//! host holds such a plugin until it's met:
//!
//! - **[`State::Needs`]:** nothing that meets it is there. The plugin
//!   doesn't start. It waits on the snapshot, with no polling and no
//!   retries, and starts as soon as something meets it, without a restart
//!   of Lodestar.
//! - **[`State::Unhealthy`]:** only a companion that's failing its health
//!   check could meet it. A running plugin is stopped (paused) and starts
//!   again once it recovers. Other plugins are unaffected.
//! - **[`State::Ok`]:** met, or the plugin needs nothing.
//!
//! The binary hands [`crate::PipelineConfig::companions`] a receiver of
//! [`Snapshot`]s from the companion manager. **It fails closed:** a
//! snapshot that is [`Snapshot::Unknown`] (not read yet, or unreadable)
//! or a sender that went away starts no gated plugin, pauses a running
//! one, and reads as [`State::Needs`] in the API. `None` turns the gate
//! off, as `setup_done: None` turns off the setup hold, so the Prowlarr
//! and download-client sources run as before until CMP-5 exists. **CMP-5
//! must wire `Some`** (runner, 2026-11-08).
//!
//! Only transitions are logged, once each (`pipeline.source_needs_companion`,
//! `pipeline.source_companion_unhealthy`, `pipeline.source_companion_ready`),
//! with the plugin and companion ids and nothing else. A companion whose
//! health flaps restarts the plugin once per flip it reports, so the
//! manager debounces health before it sends (CMP-5).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use lodestar_companions::Catalogue;
use lodestar_companions::requires::{Requirement, requirement};
use lodestar_sources::{BoxFuture, PluginError, Shutdown, SourceCtx};
use tokio::sync::watch;

use crate::{Monitor, PLUGIN_STOP_TIMEOUT, log_events};

/// A companion's health, as the companion manager last saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Healthy,
    /// Installed or detected, but failing its health check.
    Unhealthy,
}

/// What's there, as the companion manager knows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Present {
    /// Every companion installed or detected, by catalogue id, with its
    /// health. One that isn't listed is missing.
    pub companions: BTreeMap<String, Health>,
    /// A download client is configured in Settings → Integrations that
    /// Lodestar doesn't manage (one it manages is in `companions`, with
    /// its health). Its own health isn't judged here.
    pub external_client: bool,
}

/// The companion manager's latest word.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Snapshot {
    /// Not read yet, or unreadable: nothing gated starts.
    #[default]
    Unknown,
    Known(Present),
}

/// The receiver the binary hands the pipeline.
pub type Companions = watch::Receiver<Snapshot>;

/// A plugin's standing with what it needs. The companion is a catalogue
/// id: the unhealthy one, or the one to suggest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ok,
    Needs(&'static str),
    Unhealthy(&'static str),
}

/// `requirement`'s state on `snapshot`. A healthy companion that can meet
/// it, or an external client where one counts, makes it `Ok`. Otherwise
/// the first present (unhealthy) companion makes it [`State::Unhealthy`],
/// and with none it's [`State::Needs`] the suggested one. An unknown
/// snapshot reads as needing it.
pub fn evaluate(
    requirement: &Requirement,
    catalogue: Option<&Catalogue>,
    snapshot: &Snapshot,
) -> State {
    let Snapshot::Known(present) = snapshot else {
        return State::Needs(requirement.suggest);
    };
    if present.external_client && requirement.takes_external_client() {
        return State::Ok;
    }
    let ids = requirement.companions(catalogue);
    let health = |id: &str| present.companions.get(id).copied();
    if ids.iter().any(|id| health(id) == Some(Health::Healthy)) {
        return State::Ok;
    }
    match ids.iter().find(|id| health(id).is_some()) {
        Some(id) => State::Unhealthy(id),
        None => State::Needs(requirement.suggest),
    }
}

fn catalogue() -> Option<&'static Catalogue> {
    lodestar_companions::builtin().ok()
}

fn state_of(requirement: &Requirement, snapshot: &Snapshot) -> State {
    evaluate(requirement, catalogue(), snapshot)
}

impl Monitor {
    /// Plugin `plugin`'s standing now: what the Sources API shows
    /// (`companion_need`). [`State::Ok`] for a plugin that needs nothing,
    /// and for every plugin while the gate is off. A closed channel reads
    /// as unknown, as it does to the gate.
    pub fn companion_state(&self, plugin: &str) -> State {
        match (requirement(plugin), &self.companions) {
            (Some(r), Some(rx)) if rx.has_changed().is_err() => state_of(&r, &Snapshot::Unknown),
            (Some(r), Some(rx)) => state_of(&r, &rx.borrow()),
            _ => State::Ok,
        }
    }
}

/// Logs `now` if it differs from the last state logged.
fn note(plugin: &'static str, last: &mut State, now: State) {
    if *last == now {
        return;
    }
    match now {
        State::Needs(c) => {
            lodestar_log::log_event!(
                log_events::SOURCE_NEEDS_COMPANION,
                plugin = plugin,
                companion = c
            );
        }
        State::Unhealthy(c) => {
            lodestar_log::log_event!(
                log_events::SOURCE_COMPANION_UNHEALTHY,
                plugin = plugin,
                companion = c
            );
        }
        State::Ok => {
            if let State::Needs(c) | State::Unhealthy(c) = *last {
                lodestar_log::log_event!(
                    log_events::SOURCE_COMPANION_READY,
                    plugin = plugin,
                    companion = c
                );
            }
        }
    }
    *last = now;
}

/// The snapshot now, marked seen. A closed channel reads as unknown,
/// whether it closed before the gate first looked or since: the last
/// value a sender left behind never counts.
fn read(rx: &mut Companions, closed: bool) -> Snapshot {
    if closed || rx.has_changed().is_err() {
        Snapshot::Unknown
    } else {
        rx.borrow_and_update().clone()
    }
}

/// Waits for the next snapshot, or `stop`. `false` when stopped. A sender
/// dropped sets `closed`, which then reads as unknown and is never waited
/// on again.
async fn next(stop: &mut Shutdown, rx: &mut Companions, closed: &mut bool) -> bool {
    tokio::select! {
        biased;
        () = stop.requested() => false,
        changed = rx.changed(), if !*closed => {
            if changed.is_err() {
                *closed = true;
            }
            true
        }
    }
}

/// A context for one run of the plugin, from the one it was given: the
/// same source, emitters and kit (its state store too), and `shutdown`.
/// The peer-request receiver goes to the first run only; no mapped plugin
/// is a peer finder (`tests/requires.rs` checks the registry).
fn for_run(ctx: &mut SourceCtx, shutdown: Shutdown) -> SourceCtx {
    SourceCtx {
        source: ctx.source,
        emitter: ctx.emitter.clone(),
        peer_requests: ctx.peer_requests.take(),
        peer_replies: ctx.peer_replies.clone(),
        shutdown,
        kit: ctx.kit.clone(),
    }
}

/// Runs plugin `plugin` through `run`, held for what it needs when it
/// needs something and `companions` is set; otherwise just `run(ctx)`.
///
/// `running` reads `false` while it waits, and once a paused run has
/// ended (or been dropped), never while one is still winding down, so a
/// purge's stop still waits for it. It returns when the context's
/// shutdown fires (a pause winding down is cut short), or when a run ends
/// on its own, with that run's result.
pub(crate) async fn run<'a>(
    plugin: &'static str,
    companions: Option<Companions>,
    mut ctx: SourceCtx,
    running: &AtomicBool,
    mut run: impl FnMut(SourceCtx) -> BoxFuture<'a, Result<(), PluginError>>,
) -> Result<(), PluginError> {
    let (Some(req), Some(mut rx)) = (requirement(plugin), companions) else {
        return run(ctx).await;
    };
    let mut stop = ctx.shutdown.clone();
    let mut closed = false;
    // An `Ok` first start logs nothing.
    let mut logged = State::Ok;
    loop {
        // Held until met: one evaluation per snapshot sent, nothing more.
        loop {
            if stop.is_requested() {
                return Ok(());
            }
            let now = state_of(&req, &read(&mut rx, closed));
            note(plugin, &mut logged, now);
            if now == State::Ok {
                break;
            }
            running.store(false, Ordering::Relaxed);
            if !next(&mut stop, &mut rx, &mut closed).await {
                return Ok(());
            }
        }
        running.store(true, Ordering::Relaxed);
        let (pause, shutdown) = Shutdown::new();
        let mut fut = run(for_run(&mut ctx, shutdown));
        loop {
            tokio::select! {
                biased;
                r = &mut fut => return r,
                go_on = next(&mut stop, &mut rx, &mut closed) => {
                    if !go_on {
                        let _ = pause.send(true);
                        return fut.await;
                    }
                    let now = state_of(&req, &read(&mut rx, closed));
                    if now == State::Ok {
                        continue;
                    }
                    note(plugin, &mut logged, now);
                    let _ = pause.send(true);
                    // A plugin that ignores its stop is dropped (cancelled)
                    // after the stop timeout; an outright stop cuts the
                    // wait short. Its result is set aside: it was paused.
                    tokio::select! {
                        biased;
                        () = stop.requested() => return Ok(()),
                        ended = tokio::time::timeout(PLUGIN_STOP_TIMEOUT, fut) => {
                            if ended.is_err() {
                                lodestar_log::log_event!(
                                    log_events::SOURCE_STOP_TIMEOUT,
                                    plugin = plugin
                                );
                            }
                        }
                    }
                    running.store(false, Ordering::Relaxed);
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn prowlarr() -> Requirement {
        requirement("prowlarr").unwrap()
    }

    fn client() -> Requirement {
        requirement("download_clients").unwrap()
    }

    fn known(pairs: &[(&str, Health)], external_client: bool) -> Snapshot {
        Snapshot::Known(Present {
            companions: pairs.iter().map(|(k, h)| (k.to_string(), *h)).collect(),
            external_client,
        })
    }

    #[test]
    fn the_state_table() {
        use Health::*;
        let c = catalogue();
        assert!(c.is_some());
        let (p, d) = (prowlarr(), client());
        let cases: &[(Requirement, Snapshot, State)] = &[
            (p, Snapshot::Unknown, State::Needs("prowlarr")),
            (p, known(&[], false), State::Needs("prowlarr")),
            (p, known(&[("prowlarr", Healthy)], false), State::Ok),
            (
                p,
                known(&[("prowlarr", Unhealthy)], false),
                State::Unhealthy("prowlarr"),
            ),
            // Another companion, or an external client, doesn't count.
            (
                p,
                known(&[("qbittorrent", Healthy)], true),
                State::Needs("prowlarr"),
            ),
            (d, Snapshot::Unknown, State::Needs("qbittorrent")),
            (d, known(&[], false), State::Needs("qbittorrent")),
            (d, known(&[("qbittorrent", Healthy)], false), State::Ok),
            // An external client meets it, whatever a companion's health.
            (d, known(&[], true), State::Ok),
            (d, known(&[("qbittorrent", Unhealthy)], true), State::Ok),
            (
                d,
                known(&[("qbittorrent", Unhealthy)], false),
                State::Unhealthy("qbittorrent"),
            ),
            // Not a download client, and not in the catalogue.
            (
                d,
                known(&[("prowlarr", Healthy), ("transmission", Healthy)], false),
                State::Needs("qbittorrent"),
            ),
        ];
        for (r, s, want) in cases {
            assert_eq!(evaluate(r, c, s), *want, "{r:?} on {s:?}");
        }
    }

    fn health() -> impl Strategy<Value = Option<Health>> {
        prop_oneof![
            Just(None),
            Just(Some(Health::Healthy)),
            Just(Some(Health::Unhealthy))
        ]
    }

    fn snapshot() -> impl Strategy<Value = Snapshot> {
        let ids = ["qbittorrent", "prowlarr", "gluetun", "other"];
        prop_oneof![
            1 => Just(Snapshot::Unknown),
            4 => (proptest::collection::vec(health(), 4), any::<bool>()).prop_map(
                move |(hs, external_client)| {
                    Snapshot::Known(Present {
                        companions: ids
                            .iter()
                            .zip(hs)
                            .filter_map(|(id, h)| h.map(|h| (id.to_string(), h)))
                            .collect(),
                        external_client,
                    })
                }
            ),
        ]
    }

    proptest! {
        /// Over any snapshot: `Ok` exactly when a companion that can meet
        /// it is healthy, or an external client counts; otherwise it names
        /// a companion, unhealthy only if that one is there; never `Ok`
        /// on an unknown snapshot.
        #[test]
        fn ok_only_when_something_meets_it(s in snapshot(), dl in any::<bool>()) {
            let c = catalogue();
            let r = if dl { client() } else { prowlarr() };
            let ids = r.companions(c);
            let got = evaluate(&r, c, &s);
            let met = match &s {
                Snapshot::Unknown => false,
                Snapshot::Known(p) => {
                    (p.external_client && r.takes_external_client())
                        || ids.iter().any(|id| p.companions.get(*id) == Some(&Health::Healthy))
                }
            };
            prop_assert_eq!(got == State::Ok, met);
            match (got, &s) {
                (State::Ok, _) => {}
                (State::Needs(id), Snapshot::Known(p)) => {
                    prop_assert_eq!(id, r.suggest);
                    prop_assert!(ids.iter().all(|c| !p.companions.contains_key(*c)));
                }
                (State::Needs(id), Snapshot::Unknown) => prop_assert_eq!(id, r.suggest),
                (State::Unhealthy(id), Snapshot::Known(p)) => {
                    prop_assert!(ids.contains(&id));
                    prop_assert_eq!(p.companions.get(id), Some(&Health::Unhealthy));
                }
                (State::Unhealthy(_), Snapshot::Unknown) => {
                    prop_assert!(false, "unknown reads as needs")
                }
            }
        }
    }
}
