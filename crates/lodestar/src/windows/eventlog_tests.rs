//! Tests of [`super`] (`windows/eventlog.rs`), in a file of their own because
//! they declare a test-only event catalogue.

use std::sync::Arc;

use lodestar_log::{Record, Sink as _, Value};

use lodestar_api::logs::{LogSource, LogStreamConfig};

use super::*;
use crate::logging::state::Applied;
use crate::settings::logging::Logging;

mod test_events {
    lodestar_log::catalogue! {
        "eventlog-tests";
        FAILED {
            id: "test.eventlog_failed",
            component: System,
            level: Error,
            message: "test: it failed",
            fields: { name: Name, path: Path, size: Size, stage: Label, error: Error },
        }
        NOTED {
            id: "test.eventlog_noted",
            component: System,
            level: Warn,
            message: "test: noted",
            fields: {},
        }
    }
}

/// Records what it's sent; fails from the `fail_at`th message.
#[derive(Clone, Default)]
struct Fake {
    sent: Arc<Mutex<Vec<String>>>,
    fail_at: Option<usize>,
}

impl Sink for Fake {
    fn send(&mut self, message: String) -> Result<(), String> {
        let mut sent = self.sent.lock().unwrap();
        if self.fail_at.is_some_and(|n| sent.len() >= n) {
            return Err("no source".into());
        }
        sent.push(message);
        Ok(())
    }
}

impl Fake {
    fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

/// A runtime of the test's own, every data kind on in the file.
fn runtime() -> &'static Runtime {
    let l = Logging {
        data: crate::settings::logging::DataPreset::Full,
        data_until: crate::settings::logging::UNTIL_OFF,
        ..Logging::default()
    };
    let rt: &'static Runtime = Box::leak(Box::new(Runtime::new(
        Box::new(std::io::sink()),
        Applied::compute(&l, crate::logging::unix_now()),
    )));
    // Each test line counts: no folding of identical lines.
    rt.set_folding(false);
    rt
}

fn fail(rt: &Runtime) {
    let fields = [
        ("name", Value::Name("Secret.Film.2026")),
        (
            "path",
            Value::Path(std::path::Path::new("C:\\Users\\alex\\x")),
        ),
        ("size", Value::Size(42)),
        ("stage", Value::Label("open")),
        (
            "error",
            Value::Error(lodestar_log::Error::kind("timed_out")),
        ),
    ];
    rt.record(&Record {
        event: &test_events::FAILED::EVENT,
        fields: &fields,
    });
}

fn note(rt: &Runtime) {
    rt.record(&Record {
        event: &test_events::NOTED::EVENT,
        fields: &[],
    });
}

fn forward(fwd: &mut Forwarder, rt: &'static Runtime, now_ms: i64) -> Option<String> {
    let logs = Logs::new(Some(rt), LogStreamConfig::default());
    fwd.forward(|c, f, m| logs.after(c, f, m), now_ms)
}

#[test]
fn errors_go_out_with_data_values_hidden() {
    let rt = runtime();
    let fake = Fake::default();
    let mut fwd = Forwarder::new(Box::new(fake.clone()));
    note(rt);
    fail(rt);
    assert_eq!(forward(&mut fwd, rt, 0), None);
    let sent = fake.sent();
    assert_eq!(sent.len(), 1, "errors only: {sent:?}");
    assert_eq!(
        sent[0],
        "test.eventlog_failed: test: it failed name=[hidden] path=[hidden] size=[hidden] stage=open error=timed_out"
    );
    assert!(!sent[0].contains("Secret.Film") && !sent[0].contains("alex"));
    assert_eq!(forward(&mut fwd, rt, 1), None);
    assert_eq!(fake.sent().len(), 1, "each line once");
}

#[test]
fn at_most_ten_a_minute_then_a_count() {
    let rt = runtime();
    let fake = Fake::default();
    let mut fwd = Forwarder::new(Box::new(fake.clone()));
    for _ in 0..25 {
        fail(rt);
    }
    forward(&mut fwd, rt, 1_000);
    assert_eq!(fake.sent().len(), PER_MINUTE as usize);
    fail(rt);
    forward(&mut fwd, rt, 30_000);
    assert_eq!(fake.sent().len(), PER_MINUTE as usize, "same minute");
    forward(&mut fwd, rt, 61_000);
    let sent = fake.sent();
    assert_eq!(sent.len(), PER_MINUTE as usize + 1);
    assert!(sent.last().unwrap().contains("16 more errors"), "{sent:?}");
}

#[test]
fn off_sends_nothing_and_on_sends_only_whats_new() {
    let rt = runtime();
    let fake = Fake::default();
    let mut fwd = Forwarder::new(Box::new(fake.clone()));
    fwd.configure(false, 0);
    fail(rt);
    forward(&mut fwd, rt, 0);
    assert!(fake.sent().is_empty());
    fwd.configure(true, 0);
    forward(&mut fwd, rt, 1);
    assert!(fake.sent().is_empty(), "the old error stays out");
    fail(rt);
    forward(&mut fwd, rt, 2);
    assert_eq!(fake.sent().len(), 1);
}

#[test]
fn a_failure_stops_it_until_the_next_save() {
    let rt = runtime();
    let fake = Fake {
        fail_at: Some(1),
        ..Fake::default()
    };
    let mut fwd = Forwarder::new(Box::new(fake.clone()));
    fail(rt);
    fail(rt);
    assert_eq!(forward(&mut fwd, rt, 0).as_deref(), Some("no source"));
    fail(rt);
    assert_eq!(forward(&mut fwd, rt, 1), None, "noted once");
    assert_eq!(fake.sent().len(), 1);
    fwd.configure(true, 0);
    assert_eq!(forward(&mut fwd, rt, 2), None, "no save, still stopped");
    fwd.configure(true, 1);
    fail(rt);
    assert_eq!(
        forward(&mut fwd, rt, 70_000).as_deref(),
        Some("no source"),
        "tried again"
    );
}

#[test]
fn a_message_is_one_bounded_line() {
    let line = LogLine {
        component: lodestar_api::dto::logs::LogComponent::System,
        event: None,
        fields: vec![lodestar_api::dto::logs::LogField {
            kind: LogFieldKind::Untyped,
            name: "detail".into(),
            value: LogValue::Text("C:\\Users\\alex".into()),
        }],
        id: None,
        level: lodestar_api::dto::logs::LogLevel::Error,
        msg: format!("first\nsecond\u{202e} {}", "é".repeat(MAX_MESSAGE)),
        opt_in: None,
        repeated: None,
        target: Some("some_crate".into()),
        truncated: false,
        ts: String::new(),
    };
    let out = render(&line);
    assert!(out.starts_with("some_crate: firstsecond "), "{out}");
    assert!(out.len() <= MAX_MESSAGE && !out.contains('\n'));
    let short = LogLine {
        msg: "m".into(),
        ..line
    };
    assert_eq!(render(&short), "some_crate: m detail=[hidden]");
}
