//! Plugins that need a companion (architecture §18.10; plan
//! 2026-11-06-v06-companions §4.3, CMP-10).
//!
//! `lodestar_companions::requires` says which plugins need which
//! companion. Here the host holds such a plugin until it's met:
//!
//! - **[`State::Needs`]:** the companion isn't installed or detected. The
//!   plugin doesn't start. It waits on the snapshot, with no polling and
//!   no retries, and starts as soon as the companion is there and healthy,
//!   without a restart of Lodestar.
//! - **[`State::Unhealthy`]:** the companion is there but failing its
//!   health check. A running plugin is stopped (paused) and starts again
//!   once it's healthy. Other plugins are unaffected.
//! - **[`State::Ok`]:** met, or the plugin needs nothing.
//!
//! The binary hands [`crate::PipelineConfig::companions`] a receiver of
//! [`Snapshot`]s from the companion manager (CMP-5). **It fails closed:**
//! a snapshot that is [`Snapshot::Unknown`] (not read yet, or unreadable)
//! or a sender that went away starts no gated plugin and pauses a running
//! one. `None` turns the gate off, as `setup_done: None` turns off the
//! setup hold: until the manager exists nothing reports companions, and a
//! gate with nothing to read would switch off the Prowlarr and
//! download-client sources everywhere.
//!
//! Only transitions are logged, once each (`pipeline.source_needs_companion`,
//! `pipeline.source_companion_unhealthy`, `pipeline.source_companion_ready`),
//! with the plugin and companion ids and nothing else.

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

/// What the companion manager knows: every companion installed or
/// detected, by catalogue id, with its health. One that isn't listed is
/// missing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Snapshot {
    /// Not read yet, or unreadable: nothing gated starts.
    #[default]
    Unknown,
    Known(BTreeMap<String, Health>),
}

/// The receiver the binary hands the pipeline.
pub type Companions = watch::Receiver<Snapshot>;

/// A plugin's standing with the companion it needs. The companion is a
/// catalogue id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ok,
    Needs(&'static str),
    Unhealthy(&'static str),
}

/// `requirement`'s state on `snapshot`. Any healthy candidate meets it.
/// Otherwise the first present (unhealthy) one makes it
/// [`State::Unhealthy`], and with none present it's
/// [`State::Needs`] the suggested one. An unknown snapshot reads as
/// needing it.
pub fn evaluate(
    requirement: &Requirement,
    catalogue: Option<&Catalogue>,
    snapshot: &Snapshot,
) -> State {
    let Snapshot::Known(present) = snapshot else {
        return State::Needs(requirement.suggested());
    };
    let candidates = requirement.candidates(catalogue);
    let health = |id: &str| present.get(id).copied();
    if candidates
        .iter()
        .any(|id| health(id) == Some(Health::Healthy))
    {
        return State::Ok;
    }
    match candidates.iter().find(|id| health(id).is_some()) {
        Some(id) => State::Unhealthy(id),
        None => State::Needs(requirement.suggested()),
    }
}

fn catalogue() -> Option<&'static Catalogue> {
    lodestar_companions::builtin().ok()
}

fn state_of(requirement: &Requirement, snapshot: &Snapshot) -> State {
    evaluate(requirement, catalogue(), snapshot)
}

impl Monitor {
    /// Plugin `plugin`'s standing with its companion, now: what the
    /// Sources API shows (`companion_need`). [`State::Ok`] for a plugin
    /// that needs none, and for every plugin while the gate is off.
    pub fn companion_state(&self, plugin: &str) -> State {
        match (requirement(plugin), &self.companions) {
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

/// The snapshot now, marked seen. A closed channel reads as unknown.
fn read(rx: &mut Companions, closed: bool) -> Snapshot {
    if closed {
        Snapshot::Unknown
    } else {
        rx.borrow_and_update().clone()
    }
}

/// Waits for the next snapshot, or `stop`. `false` when stopped. A sender
/// dropped sets `closed`, which then reads as unknown.
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

/// Runs plugin `plugin` through `run`, held for its companion when it
/// needs one and `companions` is set; otherwise just `run(ctx)`.
///
/// `running` reads `false` while it waits or is paused. It returns when
/// the context's shutdown fires (a pause winding down is cut short), or
/// when a run ends on its own, with that run's result.
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
                    running.store(false, Ordering::Relaxed);
                    let _ = pause.send(true);
                    // A plugin that ignores its stop is dropped (cancelled)
                    // after the stop timeout; an outright stop cuts the
                    // wait short. Its result is set aside: it was paused.
                    tokio::select! {
                        biased;
                        () = stop.requested() => return Ok(()),
                        _ = tokio::time::timeout(PLUGIN_STOP_TIMEOUT, fut) => {}
                    }
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use lodestar_companions::catalogue::Need;
    use proptest::prelude::*;

    use super::*;

    const PROWLARR: Requirement = Requirement::Companion("prowlarr");
    const CLIENT: Requirement = Requirement::AnyOf {
        need: Need::DownloadClient,
        suggest: "qbittorrent",
    };

    fn known(pairs: &[(&str, Health)]) -> Snapshot {
        Snapshot::Known(pairs.iter().map(|(k, h)| (k.to_string(), *h)).collect())
    }

    #[test]
    fn the_state_table() {
        use Health::*;
        let c = catalogue();
        assert!(c.is_some());
        let cases: &[(Requirement, Snapshot, State)] = &[
            (PROWLARR, Snapshot::Unknown, State::Needs("prowlarr")),
            (PROWLARR, known(&[]), State::Needs("prowlarr")),
            (PROWLARR, known(&[("prowlarr", Healthy)]), State::Ok),
            (
                PROWLARR,
                known(&[("prowlarr", Unhealthy)]),
                State::Unhealthy("prowlarr"),
            ),
            // Another companion doesn't count.
            (
                PROWLARR,
                known(&[("qbittorrent", Healthy)]),
                State::Needs("prowlarr"),
            ),
            (CLIENT, Snapshot::Unknown, State::Needs("qbittorrent")),
            (CLIENT, known(&[("qbittorrent", Healthy)]), State::Ok),
            (
                CLIENT,
                known(&[("qbittorrent", Unhealthy)]),
                State::Unhealthy("qbittorrent"),
            ),
            // Not a download client, and not in the catalogue.
            (
                CLIENT,
                known(&[("prowlarr", Healthy), ("transmission", Healthy)]),
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
            4 => proptest::collection::vec(health(), 4).prop_map(move |hs| {
                Snapshot::Known(
                    ids.iter()
                        .zip(hs)
                        .filter_map(|(id, h)| h.map(|h| (id.to_string(), h)))
                        .collect(),
                )
            }),
        ]
    }

    proptest! {
        /// Over any snapshot: `Ok` exactly when a candidate is healthy;
        /// otherwise it names a candidate, unhealthy only if that one is
        /// there; never `Ok` on an unknown snapshot.
        #[test]
        fn ok_only_when_a_candidate_is_healthy(s in snapshot(), any in any::<bool>()) {
            let c = catalogue();
            let r = if any { CLIENT } else { PROWLARR };
            let cands = r.candidates(c);
            let got = evaluate(&r, c, &s);
            let healthy = match &s {
                Snapshot::Unknown => false,
                Snapshot::Known(m) => cands.iter().any(|id| m.get(*id) == Some(&Health::Healthy)),
            };
            prop_assert_eq!(got == State::Ok, healthy);
            match (got, &s) {
                (State::Ok, _) => {}
                (State::Needs(id), Snapshot::Known(m)) => {
                    prop_assert_eq!(id, r.suggested());
                    prop_assert!(cands.iter().all(|c| !m.contains_key(*c)));
                }
                (State::Needs(id), Snapshot::Unknown) => prop_assert_eq!(id, r.suggested()),
                (State::Unhealthy(id), Snapshot::Known(m)) => {
                    prop_assert!(cands.contains(&id));
                    prop_assert_eq!(m.get(id), Some(&Health::Unhealthy));
                }
                (State::Unhealthy(_), Snapshot::Unknown) => prop_assert!(false, "unknown reads as needs"),
            }
        }
    }
}
