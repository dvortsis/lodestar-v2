//! Logging, live while the server runs: `main` installs the runtime
//! through [`init`] (the console, a panic hook and the `tracing`
//! subscriber for untyped lines), and [`follow`] applies the `logging`
//! section: first at start, which opens the log file, then at every save
//! and whenever a data kind's or a timed verbosity's deadline passes.

use std::io::Write;
use std::sync::OnceLock;

use lodestar_log::{Kind, log_event};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry, reload};

use super::Section;
use super::logging::{Logging, UNTIL_OFF};
use crate::config::Config;
use crate::logging::state::Applied;
use crate::logging::{self, Runtime, Switched, events, unix_now};

/// The installed subscriber's filter (untyped lines only).
static FILTER: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();

/// `filter` as a `tracing` filter, or why it isn't one.
pub fn parse(filter: &str) -> Result<EnvFilter, String> {
    EnvFilter::try_new(filter).map_err(|e| format!("Not a log filter: {e}."))
}

/// Installs logging with the console on stderr. `log` is `config.toml`'s
/// `log` or `LODESTAR_LOG` (§12.1's aliases); until [`follow`] applies the
/// saved section, everything else is at its default (Private).
pub fn init(log: &str) {
    init_with_writer(log, Box::new(std::io::stderr()));
}

/// [`init`] with the console going to `console` instead: the Windows
/// service's log (M6-win), or a test's buffer.
pub fn init_with_writer(log: &str, console: Box<dyn Write + Send>) {
    let section = Logging::from_config(&Config {
        log: log.to_string(),
        ..Config::default()
    });
    let applied = Applied::compute(&section, unix_now());
    let filter = parse(&applied.filter).unwrap_or_else(|_| EnvFilter::new("warn"));
    let first = logging::runtime().is_none();
    logging::install(console, applied);
    if !first {
        return;
    }
    let (layer, handle) = reload::Layer::new(filter);
    if tracing_subscriber::registry()
        .with(layer)
        .with(logging::Forward)
        .try_init()
        .is_ok()
    {
        let _ = FILTER.set(handle);
    }
    std::panic::set_hook(Box::new(|info| {
        // The message can hold anything (a name, a path): only where it
        // happened is written.
        match info.location() {
            Some(l) => log_event!(events::APP_CRASH, location = lodestar_log::Location(l)),
            None => log_event!(events::APP_CRASH),
        }
    }));
    log_event!(events::APP_START, version = env!("CARGO_PKG_VERSION"));
}

/// Replaces the filter behind `handle`.
pub fn reload<S>(handle: &reload::Handle<EnvFilter, S>, filter: &str) -> Result<(), String> {
    let filter = parse(filter)?;
    handle.reload(filter).map_err(|e| e.to_string())
}

/// Why a state is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    Start,
    Saved,
    Deadline,
}

/// Each switch as the required event says it: the kind, its new state,
/// and its deadline when it turned on until a time.
fn switch_lines(switched: &[Switched], why: Why) -> Vec<(&'static str, &'static str, Option<u64>)> {
    switched
        .iter()
        .map(|s| {
            let state = match (s.on, why) {
                (true, _) if s.until == UNTIL_OFF => "on_until_off",
                (true, _) => "on",
                (false, Why::Deadline) => "expired",
                (false, _) => "off",
            };
            let until = (s.on && s.until > 0).then_some(s.until as u64);
            (s.kind.name(), state, until)
        })
        .collect()
}

/// Logs the required switch event for each kind that changed.
fn report(switched: &[Switched], why: Why) {
    for (kind, state, until) in switch_lines(switched, why) {
        match until {
            Some(u) => log_event!(
                events::LOGGING_KIND_SWITCHED,
                kind = kind,
                state = state,
                until = lodestar_log::Count(u)
            ),
            None => log_event!(events::LOGGING_KIND_SWITCHED, kind = kind, state = state),
        }
    }
}

/// Deadlines that passed while Lodestar wasn't running: their kinds are
/// off, and the start says so.
fn expired_while_down(l: &Logging, now: i64) -> Vec<Switched> {
    let mut deadlines: Vec<(Kind, i64)> = vec![
        (Kind::PeerActivity, l.peer_activity_until),
        (Kind::FilterChange, l.change_details_until),
    ];
    for (name, until) in &l.kinds {
        if let Some(k) = Kind::parse(name) {
            deadlines.push((k, *until));
        }
    }
    if l.data != super::logging::DataPreset::Private {
        for k in Kind::ALL
            .into_iter()
            .filter(|k| k.identifying() && l.data.has(*k))
        {
            if !l.kinds.contains_key(k.name()) {
                deadlines.push((k, l.data_until));
            }
        }
    }
    deadlines
        .into_iter()
        .filter(|(_, until)| *until > 0 && *until <= now)
        .map(|(kind, _)| Switched {
            kind,
            on: false,
            until: 0,
        })
        .collect()
}

/// Applies the `logging` section through `runtime`, and `apply_filter` for
/// untyped lines: now, at each change, and at each deadline.
pub fn follow_with(
    mut rx: watch::Receiver<Logging>,
    runtime: Option<&'static Runtime>,
    mut apply_filter: impl FnMut(&str) -> Result<(), String> + Send + 'static,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut why = Why::Start;
        let mut filter: Option<String> = None;
        let mut labels: Option<(&str, &str)> = None;
        loop {
            let section = rx.borrow_and_update().clone();
            let now = unix_now();
            let applied = Applied::compute(&section, now);
            if filter.as_ref() != Some(&applied.filter) {
                if let Err(e) = apply_filter(&applied.filter) {
                    let _ = e;
                    log_event!(
                        events::LOGGING_FILTER_NOT_APPLIED,
                        error = lodestar_log::Error::kind("not_a_filter")
                    );
                }
                filter = Some(applied.filter.clone());
            }
            let next = applied.next_deadline;
            let now_labels = (applied.verbosity, applied.data);
            if let Some(rt) = runtime {
                let switched = rt.apply(applied);
                if why == Why::Start {
                    rt.open_file();
                    report(&expired_while_down(&section, now), Why::Deadline);
                }
                report(&switched, why);
            }
            if labels != Some(now_labels) {
                log_event!(
                    events::LOGGING_APPLIED,
                    verbosity = now_labels.0,
                    data = now_labels.1
                );
                labels = Some(now_labels);
            }
            let wake = next.map(|n| {
                let secs = (n - unix_now()).max(0) as u64;
                tokio::time::Instant::now()
                    + std::time::Duration::from_secs(secs)
                    + std::time::Duration::from_millis(50)
            });
            tokio::select! {
                changed = rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    why = Why::Saved;
                }
                () = async {
                    match wake {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => why = Why::Deadline,
            }
        }
    })
}

/// [`follow_with`] the runtime and the subscriber [`init`] installed;
/// without them (tests, tools) changes are only saved.
pub fn follow(rx: watch::Receiver<Logging>) -> JoinHandle<()> {
    follow_with(rx, logging::runtime(), |filter| match FILTER.get() {
        Some(handle) => reload(handle, filter),
        None => Ok(()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::tests::Buf;

    fn switched(kind: Kind, on: bool, until: i64) -> Switched {
        Switched { kind, on, until }
    }

    #[test]
    fn switches_say_how_they_happened() {
        let s = [
            switched(Kind::Name, true, 1_800_000_000),
            switched(Kind::Hash, true, UNTIL_OFF),
            switched(Kind::Peer, false, 0),
        ];
        assert_eq!(
            switch_lines(&s, Why::Saved),
            [
                ("name", "on", Some(1_800_000_000)),
                ("hash", "on_until_off", None),
                ("peer", "off", None)
            ]
        );
        assert_eq!(
            switch_lines(&s[2..], Why::Deadline),
            [("peer", "expired", None)]
        );
    }

    #[test]
    fn deadlines_that_passed_while_stopped_are_found() {
        let now = 1_800_000_000;
        let mut l = Logging {
            peer_activity_until: now - 60,
            change_details_until: now + 60,
            data: super::super::logging::DataPreset::Hashes,
            data_until: now - 1,
            ..Logging::default()
        };
        l.kinds.insert("url".into(), now - 5);
        l.kinds.insert("query".into(), UNTIL_OFF);
        let mut kinds: Vec<Kind> = expired_while_down(&l, now)
            .into_iter()
            .map(|s| s.kind)
            .collect();
        kinds.sort();
        assert_eq!(kinds, [Kind::Hash, Kind::Url, Kind::PeerActivity]);
    }

    /// A runtime of the test's own: never the process's sink, so other
    /// tests' log captures are untouched.
    fn private_runtime(dir: &std::path::Path) -> &'static Runtime {
        let l = Logging {
            file_folder: dir.display().to_string(),
            ..Logging::default()
        };
        Box::leak(Box::new(Runtime::new(
            Box::new(Buf::default()),
            Applied::compute(&l, unix_now()),
        )))
    }

    #[tokio::test]
    async fn deadlines_expire_without_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let rt = private_runtime(dir.path());
        let mut l = Logging {
            file_folder: dir.path().display().to_string(),
            ..Logging::default()
        };
        l.kinds.insert("name".into(), unix_now() + 1);
        let (_tx, rx) = watch::channel(l);
        let task = follow_with(rx, Some(rt), |_| Ok(()));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(rt.applied().file_mask.has(Kind::Name));
        assert!(
            dir.path().join(crate::logging::CURRENT).exists(),
            "the file opened"
        );
        tokio::time::sleep(std::time::Duration::from_millis(2_200)).await;
        assert!(!rt.applied().file_mask.has(Kind::Name), "off with no save");
        task.abort();
    }

    #[tokio::test]
    async fn a_save_applies_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let rt = private_runtime(dir.path());
        let mut l = Logging {
            file_folder: dir.path().display().to_string(),
            ..Logging::default()
        };
        let (tx, rx) = watch::channel(l.clone());
        let task = follow_with(rx, Some(rt), |_| Ok(()));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!rt.applied().file_mask.has(Kind::Hash));
        l.data = super::super::logging::DataPreset::Hashes;
        l.data_until = UNTIL_OFF;
        tx.send(l).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(rt.applied().file_mask.has(Kind::Hash));
        assert_eq!(rt.applied().data, "hashes");
        task.abort();
    }
}
