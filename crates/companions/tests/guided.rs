//! CMP-5g's watch loop over companions Lodestar doesn't supervise: each is
//! checked when added and on every tick, a healthy one goes down only
//! after three failures in a row (Q7), one that leaves the list leaves the
//! report, and a changed target starts afresh. Waits are bounded at 10 s.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_companions::adapter::{
    AdapterError, BoxFuture, GuardedHttp, Health, HealthStatus, ProbeRequest, ProbeResponse, Target,
};
use lodestar_companions::inputs::{CompanionId, PortNumber};
use lodestar_companions::watch::{self, Checker, Seen, Watched};
use tokio::sync::watch as channel;

const DEADLINE: Duration = Duration::from_secs(10);

/// Never used: the fake health answers without it.
struct NoHttp;

impl GuardedHttp for NoHttp {
    fn send<'a>(
        &'a self,
        _: ProbeRequest<'a>,
    ) -> BoxFuture<'a, Result<ProbeResponse, AdapterError>> {
        Box::pin(async { Err(AdapterError::Refused("test")) })
    }
}

/// Answers each companion with the status set for it, counting checks.
#[derive(Default)]
struct Script {
    status: Mutex<HashMap<String, HealthStatus>>,
    checks: Mutex<HashMap<String, u32>>,
}

struct Fake(CompanionId, Arc<Script>);

impl Health for Fake {
    fn companion(&self) -> &CompanionId {
        &self.0
    }
    fn check<'a>(&'a self, _: &'a dyn GuardedHttp, _: &'a Target) -> BoxFuture<'a, HealthStatus> {
        let id = self.0.as_str().to_owned();
        *self.1.checks.lock().unwrap().entry(id.clone()).or_default() += 1;
        let s = *self
            .1
            .status
            .lock()
            .unwrap()
            .get(&id)
            .unwrap_or(&HealthStatus::Healthy);
        Box::pin(async move { s })
    }
}

fn id(s: &str) -> CompanionId {
    CompanionId::parse(s).unwrap()
}

fn loopback(p: u16) -> Target {
    Target::Loopback(PortNumber::new(p).unwrap())
}

impl Script {
    fn set(&self, id: &str, s: HealthStatus) {
        self.status.lock().unwrap().insert(id.into(), s);
    }
    fn checks(&self, id: &str) -> u32 {
        *self.checks.lock().unwrap().get(id).unwrap_or(&0)
    }
}

fn checker(script: &Arc<Script>) -> Checker {
    let s = script.clone();
    Arc::new(move |id: &CompanionId| -> Option<Box<dyn Health>> {
        // gluetun has no health here, as a companion without an adapter.
        (id.as_str() != "gluetun").then(|| Box::new(Fake(id.clone(), s.clone())) as Box<dyn Health>)
    })
}

async fn until(rx: &mut channel::Receiver<Watched>, what: &str, f: impl Fn(&Watched) -> bool) {
    let got = tokio::time::timeout(DEADLINE, rx.wait_for(|w| f(w)))
        .await
        .map(|r| r.map(|_| ()));
    match got {
        Ok(Ok(())) => {}
        Ok(Err(_)) => panic!("the loop stopped waiting for {what}"),
        Err(_) => panic!("timed out waiting for {what}; saw {:?}", *rx.borrow()),
    }
}

async fn checks_reach(script: &Script, id: &str, n: u32) {
    tokio::time::timeout(DEADLINE, async {
        while script.checks(id) < n {
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{id} wasn't checked {n} times; was {}", script.checks(id)));
}

#[tokio::test]
async fn a_healthy_companion_goes_down_only_after_three_failures() {
    let script = Arc::new(Script::default());
    let (list_tx, list) = channel::channel(vec![(id("prowlarr"), loopback(9696))]);
    let (mut rx, task) = watch::spawn(
        Arc::new(NoHttp),
        checker(&script),
        list,
        Duration::from_millis(10),
    );
    until(&mut rx, "healthy", |w| {
        w.get(&id("prowlarr")) == Some(&Seen::Healthy)
    })
    .await;
    let before = script.checks("prowlarr");
    script.set("prowlarr", HealthStatus::Unreachable);
    // Two more failed checks keep it healthy.
    checks_reach(&script, "prowlarr", before + 2).await;
    // A check is published only after it ran, so the report still says
    // healthy until the third failure.
    until(&mut rx, "missing", |w| {
        w.get(&id("prowlarr")) == Some(&Seen::Missing)
    })
    .await;
    assert!(script.checks("prowlarr") >= before + FAILURES_SEEN);
    script.set("prowlarr", HealthStatus::Healthy);
    until(&mut rx, "healthy again", |w| {
        w.get(&id("prowlarr")) == Some(&Seen::Healthy)
    })
    .await;
    drop(list_tx);
    tokio::time::timeout(DEADLINE, task).await.unwrap().unwrap();
}

const FAILURES_SEEN: u32 = watch::FAILURES;

#[tokio::test]
async fn the_report_follows_the_list() {
    let script = Arc::new(Script::default());
    script.set("qbittorrent", HealthStatus::Unhealthy);
    let (list_tx, list) = channel::channel(vec![(id("prowlarr"), loopback(9696))]);
    let (mut rx, task) = watch::spawn(
        Arc::new(NoHttp),
        checker(&script),
        list,
        // Long: a change of list is checked at once, not on the tick.
        Duration::from_secs(3600),
    );
    until(&mut rx, "prowlarr", |w| w.len() == 1).await;
    list_tx.send_replace(vec![
        (id("prowlarr"), loopback(9696)),
        (id("qbittorrent"), loopback(8080)),
        (id("gluetun"), loopback(8000)),
    ]);
    until(&mut rx, "qbittorrent added", |w| {
        w.get(&id("qbittorrent")) == Some(&Seen::Unhealthy)
    })
    .await;
    // No adapter, never listed.
    assert!(!rx.borrow().contains_key(&id("gluetun")));
    list_tx.send_replace(vec![(id("qbittorrent"), loopback(8080))]);
    until(&mut rx, "prowlarr removed", |w| {
        !w.contains_key(&id("prowlarr"))
    })
    .await;
    task.abort();
}

#[tokio::test]
async fn a_changed_target_starts_its_debounce_afresh() {
    let script = Arc::new(Script::default());
    let (list_tx, list) = channel::channel(vec![(id("prowlarr"), loopback(9696))]);
    let (mut rx, task) = watch::spawn(
        Arc::new(NoHttp),
        checker(&script),
        list,
        Duration::from_secs(3600),
    );
    until(&mut rx, "healthy", |w| {
        w.get(&id("prowlarr")) == Some(&Seen::Healthy)
    })
    .await;
    // On the new target its first failure counts at once: it was never
    // seen healthy there.
    script.set("prowlarr", HealthStatus::Unreachable);
    let url = lodestar_companions::adapter::EnteredUrl::parse("http://prowlarr.lan:9696/").unwrap();
    list_tx.send_replace(vec![(id("prowlarr"), Target::Entered(url))]);
    until(&mut rx, "missing at once", |w| {
        w.get(&id("prowlarr")) == Some(&Seen::Missing)
    })
    .await;
    task.abort();
}
