//! The runtime against its outputs: what each one shows, the start-up
//! buffer, a full disk, and deadlines that pass.

use std::io::Write;
use std::sync::{Arc, Mutex};

use lodestar_log::{Record, Sink, Value};

use super::state::Applied;
use super::*;
use crate::settings::logging::{ConsoleDetail, ConsoleFormat, DataPreset, Logging, UNTIL_OFF};

/// A console that tests read back.
#[derive(Clone, Default)]
pub struct Buf(pub Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Buf {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

const HASH: [u8; 20] = [0xc9; 20];
const NAME: &str = "Fixture.Name.2026";
const PEER: &str = "203.0.113.9:6881";

fn record_all(rt: &Runtime) {
    let peer = PEER.parse().unwrap();
    let fields = [
        ("torrent", Value::Torrent(&HASH)),
        ("name", Value::Name(NAME)),
        ("peer", Value::Peer(peer)),
        ("size", Value::Size(42)),
    ];
    rt.record(&Record {
        event: &test_events::EVERYTHING::EVENT,
        fields: &fields,
    });
    let detail = [("change", Value::Detail("add_rule query=\"secret term\""))];
    rt.record(&Record {
        event: &test_events::DETAIL::EVENT,
        fields: &detail,
    });
}

mod test_events {
    lodestar_log::catalogue! {
        "tests";
        EVERYTHING {
            id: "test.everything",
            component: Fetch,
            level: Info,
            message: "everything",
            fields: { torrent: Hash, name: Name, peer: Peer, size: Size },
        }
        DETAIL {
            id: "test.detail",
            component: Filters,
            level: Info,
            opt_in: FilterChange,
            message: "detail",
            fields: { change: FilterChange },
        }
    }
}

fn section(f: impl FnOnce(&mut Logging)) -> Logging {
    let mut l = Logging::default();
    f(&mut l);
    l
}

fn runtime(dir: &std::path::Path, l: &Logging) -> (Runtime, Buf) {
    let console = Buf::default();
    let mut l = l.clone();
    l.file_folder = dir.join("logs").display().to_string();
    let rt = Runtime::new(Box::new(console.clone()), Applied::compute(&l, unix_now()));
    // These tests count identical lines.
    rt.set_folding(false);
    rt.open_file();
    (rt, console)
}

fn file_text(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("logs").join(file::CURRENT)).unwrap_or_default()
}

#[test]
fn private_shows_no_torrent_and_no_peer_anywhere() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, console) = runtime(dir.path(), &Logging::default());
    record_all(&rt);
    let (c, f) = (console.text(), file_text(dir.path()));
    for out in [&c, &f] {
        assert!(
            !out.contains("c9c9c9") && !out.contains(NAME) && !out.contains("203.0.113.9"),
            "{out}"
        );
        assert!(out.contains("ref:"), "a reference stands in: {out}");
        assert!(!out.contains("secret term"), "the opt-in is off: {out}");
    }
    assert!(c.contains("size=42"), "{c}");
    // The file marks every field with its kind.
    let line: serde_json::Value = serde_json::from_str(f.lines().next().unwrap()).unwrap();
    assert_eq!(
        line["fields"]["name"],
        serde_json::json!({"kind": "name", "value": "[hidden]"})
    );
    assert_eq!(
        line["fields"]["size"],
        serde_json::json!({"kind": "size", "value": 42})
    );
    assert_eq!(line["event"], "test.everything");
    assert_eq!(line["component"], "fetch");
    let r = line["fields"]["torrent"]["value"].as_str().unwrap();
    // Stable within a run.
    record_all(&rt);
    assert!(file_text(dir.path()).matches(r).count() >= 2);
}

#[test]
fn the_console_stays_private_under_a_full_file() {
    let dir = tempfile::tempdir().unwrap();
    let l = section(|l| {
        l.data = DataPreset::Full;
        l.data_until = UNTIL_OFF;
        l.peer_activity_until = unix_now() + 600;
        l.change_details_until = unix_now() + 600;
    });
    let (rt, console) = runtime(dir.path(), &l);
    record_all(&rt);
    let (c, f) = (console.text(), file_text(dir.path()));
    assert!(
        f.contains(NAME) && f.contains(&"c9".repeat(20)) && f.contains(PEER),
        "{f}"
    );
    assert!(
        f.contains("secret term"),
        "the opt-in event is in the file: {f}"
    );
    assert!(
        !c.contains(NAME) && !c.contains("c9c9c9") && !c.contains("203.0.113.9"),
        "{c}"
    );
    assert!(
        !c.contains("secret term") && !c.contains("test.detail"),
        "opt-ins never reach the console: {c}"
    );
    // The details index knows.
    let index = std::fs::read_to_string(dir.path().join("logs").join(file::INDEX)).unwrap();
    assert!(
        index.contains("\"details\":true") && index.contains("\"opt_in\":true"),
        "{index}"
    );

    // "Same as the file": the kinds, but never the opt-ins.
    let dir = tempfile::tempdir().unwrap();
    let mut same = l.clone();
    same.console_detail = ConsoleDetail::File;
    same.console_format = ConsoleFormat::Json;
    let (rt, console) = runtime(dir.path(), &same);
    record_all(&rt);
    let c = console.text();
    assert!(c.contains(NAME), "{c}");
    assert!(!c.contains("secret term"), "{c}");
    // Peer activity is an opt-in: on the console a peer beside a torrent
    // stays hidden.
    assert!(!c.contains("203.0.113.9"), "{c}");
    assert!(c.starts_with('{'), "JSON: {c}");
}

#[test]
fn one_kind_at_a_time_shows_only_that_kind() {
    for kind in [Kind::Hash, Kind::Name, Kind::Peer] {
        let dir = tempfile::tempdir().unwrap();
        let l = section(|l| {
            l.kinds.insert(kind.name().into(), unix_now() + 600);
        });
        let (rt, _) = runtime(dir.path(), &l);
        record_all(&rt);
        let f = file_text(dir.path());
        assert_eq!(
            f.contains(&"c9".repeat(20)),
            kind == Kind::Hash,
            "{kind:?}: {f}"
        );
        assert_eq!(f.contains(NAME), kind == Kind::Name, "{kind:?}: {f}");
        // A peer next to a torrent needs peer activity, so peers alone
        // never show it here.
        assert!(!f.contains("203.0.113.9"), "{kind:?}: {f}");
    }
}

#[test]
fn start_up_lines_are_buffered_into_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let console = Buf::default();
    let l = section(|l| l.file_folder = dir.path().join("logs").display().to_string());
    let rt = Runtime::new(Box::new(console.clone()), Applied::compute(&l, unix_now()));
    rt.set_folding(false);
    record_all(&rt);
    assert!(
        !dir.path().join("logs").join(file::CURRENT).exists(),
        "not open yet"
    );
    assert!(
        console.text().contains("everything"),
        "the console has it at once"
    );
    rt.open_file();
    let f = file_text(dir.path());
    assert!(f.contains("test.everything"), "{f}");
    // Then lines go straight in.
    record_all(&rt);
    assert_eq!(file_text(dir.path()).matches("test.everything").count(), 2);
}

#[test]
fn the_buffer_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let l = section(|l| l.file_folder = dir.path().join("logs").display().to_string());
    let rt = Runtime::new(Box::new(Buf::default()), Applied::compute(&l, unix_now()));
    rt.set_folding(false);
    for _ in 0..STARTUP_LINES + 10 {
        record_all(&rt);
    }
    match &*lock(&rt.file) {
        FileState::Buffering { lines, dropped, .. } => {
            assert_eq!(lines.len(), STARTUP_LINES);
            assert!(*dropped >= 10);
        }
        _ => panic!("still buffering"),
    }
}

#[test]
fn a_full_disk_stops_the_file_not_lodestar() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, console) = runtime(dir.path(), &Logging::default());
    match &mut *lock(&rt.file) {
        FileState::Open(sink) => sink.full = true,
        _ => panic!("open"),
    }
    record_all(&rt);
    assert!(matches!(*lock(&rt.file), FileState::Stopped));
    // Still logging to the console, and nothing panics.
    record_all(&rt);
    assert!(console.text().matches("everything").count() >= 2);
}

#[test]
fn turning_the_file_off_and_on() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, _) = runtime(dir.path(), &Logging::default());
    let mut l = Logging {
        file_folder: dir.path().join("logs").display().to_string(),
        file_enabled: false,
        ..Logging::default()
    };
    rt.apply(Applied::compute(&l, unix_now()));
    record_all(&rt);
    assert!(file_text(dir.path()).is_empty());
    l.file_enabled = true;
    rt.apply(Applied::compute(&l, unix_now()));
    record_all(&rt);
    assert!(file_text(dir.path()).contains("test.everything"));
}
