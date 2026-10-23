//! `/logs` over a real runtime: the ring and its ids, purges, folding,
//! and history's join with the files.

use std::time::Duration;

use lodestar_api::logs::{
    HistoryQuery, LineFilter, LogLine, LogSource, LogStreamConfig, LogValue, LogsError,
};
use lodestar_log::{Record, Sink, Value};

use super::ring::Ring;
use super::state::Applied;
use super::tests::Buf;
use super::*;
use crate::settings::logging::{DataPreset, Logging, UNTIL_OFF};

mod ev {
    lodestar_log::catalogue! {
        "source-tests";
        LINE {
            id: "srctest.line",
            component: Fetch,
            level: Info,
            message: "hist-line",
            fields: { n: Size, name: Name },
        }
        FLOOD {
            id: "srctest.flood",
            component: Fetch,
            level: Warn,
            message: "flood",
            fields: { name: Name },
        }
        NEEDED {
            id: "srctest.needed",
            component: System,
            level: Info,
            required: true,
            message: "needed",
            fields: {},
        }
        DETAIL {
            id: "srctest.detail",
            component: Filters,
            level: Info,
            opt_in: FilterChange,
            message: "detail",
            fields: { change: FilterChange },
        }
    }
}

const NAME: &str = "Source.Fixture.Name";

fn line(rt: &Runtime, n: u64, name: &str) {
    let fields = [("n", Value::Size(n)), ("name", Value::Name(name))];
    rt.record(&Record {
        event: &ev::LINE::EVENT,
        fields: &fields,
    });
}

fn section(dir: &std::path::Path, f: impl FnOnce(&mut Logging)) -> Logging {
    let mut l = Logging {
        file_folder: dir.join("logs").display().to_string(),
        ..Logging::default()
    };
    f(&mut l);
    l
}

/// A runtime of its own (never installed), its file open.
fn runtime(l: &Logging, ring: Option<usize>, fold: bool) -> (&'static Runtime, Buf) {
    let console = Buf::default();
    let mut rt = Runtime::new(Box::new(console.clone()), Applied::compute(l, unix_now()));
    if let Some(n) = ring {
        rt.ring = Ring::with_limits(rt.applied().file_mask, n, ring::MAX_BYTES);
    }
    rt.set_folding(fold);
    let rt: &'static Runtime = Box::leak(Box::new(rt));
    rt.open_file();
    (rt, console)
}

fn logs(rt: &'static Runtime) -> Logs {
    Logs::new(Some(rt), LogStreamConfig::default())
}

fn settle(rt: &Runtime) {
    if let FileState::Open(sink) = &*lock(&rt.file) {
        sink.settle();
    }
}

fn file_text(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("logs").join(file::CURRENT)).unwrap_or_default()
}

fn query(text: &str, limit: usize, cursor: Option<String>) -> HistoryQuery {
    HistoryQuery {
        filter: LineFilter {
            text: Some(text.to_lowercase()),
            ..LineFilter::default()
        },
        since: None,
        until: None,
        limit,
        cursor,
    }
}

fn number(l: &LogLine) -> u64 {
    l.fields
        .iter()
        .find_map(|f| match (&*f.name, &f.value) {
            ("n", LogValue::Number(n)) => Some(*n),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no n: {l:?}"))
}

/// Every page of history for `text`, newest first.
fn all_pages(l: &Logs, text: &str, limit: usize) -> Vec<LogLine> {
    let mut out = Vec::new();
    let mut cursor = None;
    for _ in 0..10_000 {
        let page = l.history(&query(text, limit, cursor)).unwrap();
        out.extend(page.lines);
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => return out,
        }
    }
    panic!("paging didn't end");
}

#[test]
fn every_line_has_one_id_in_the_ring_and_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, _) = runtime(&section(dir.path(), |_| {}), None, true);
    for n in 0..10 {
        line(rt, n, "x");
    }
    let ring = rt.ring().snapshot();
    let ids: Vec<String> = file_text(dir.path())
        .lines()
        .filter(|l| l.contains("srctest.line"))
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["id"].as_str().unwrap().to_string()
        })
        .collect();
    let ours: Vec<String> = ring
        .iter()
        .filter(|e| e.source.name() == "srctest.line")
        .map(|e| rt.id(e.seq))
        .collect();
    assert_eq!(ids, ours);
    assert_eq!(ids.len(), 10);
    assert!(ids[0].starts_with(rt.epoch()) && rt.epoch().len() == 16);
}

#[test]
fn the_setup_token_never_enters_the_ring_or_history() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, console) = runtime(&section(dir.path(), |_| {}), None, true);
    let fields = [
        ("token", Value::Secret("TOKEN-ABCD-1234")),
        ("address", Value::Label("http://x")),
    ];
    rt.record(&Record {
        event: &events::AUTH_SETUP_TOKEN::EVENT,
        fields: &fields,
    });
    rt.record_untyped(&format::Untyped {
        level: Level::Warn,
        target: "lodestar::services".into(),
        component: "system",
        message: "first run: open x and enter setup token TOKEN-EFGH-5678 to create".into(),
        fields: vec![],
    });
    assert!(
        console.text().contains("TOKEN-ABCD-1234"),
        "the console has it"
    );
    assert!(
        file_text(dir.path()).contains("TOKEN-EFGH-5678"),
        "the file has it"
    );
    let dump = format!("{:?}", rt.ring().snapshot());
    assert!(!dump.contains("TOKEN-"), "{dump}");
    let l = logs(rt);
    let page = l.history(&query("", 1000, None)).unwrap();
    let text = serde_json::to_string(&page).unwrap();
    assert!(!text.contains("TOKEN-"), "{text}");
    assert!(text.contains("[redacted]"));
    // Past the ring: from the file, still redacted.
    let (rt2, _) = runtime(&section(dir.path(), |_| {}), Some(1), true);
    rt2.record_untyped(&format::Untyped {
        level: Level::Warn,
        target: "t".into(),
        component: "system",
        message: "filler".into(),
        fields: vec![],
    });
    let page = logs(rt2)
        .history(&query("setup token", 1000, None))
        .unwrap();
    let text = serde_json::to_string(&page).unwrap();
    assert!(
        !text.contains("TOKEN-") && text.contains("[redacted]"),
        "{text}"
    );
}

#[test]
fn turning_a_kind_off_purges_the_ring_by_save_and_by_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let now = unix_now();
    let full = section(dir.path(), |l| {
        l.data = DataPreset::Full;
        l.data_until = UNTIL_OFF;
        l.change_details_until = now + 600;
    });
    let (rt, _) = runtime(&full, None, true);
    line(rt, 1, NAME);
    let detail = [("change", Value::Detail("rule canary-term"))];
    rt.record(&Record {
        event: &ev::DETAIL::EVENT,
        fields: &detail,
    });
    let dump = format!("{:?}", rt.ring().snapshot());
    assert!(dump.contains(NAME) && dump.contains("canary-term"));
    // A save to Private: names rewritten, the opt-in's line removed.
    let switched = rt.apply(Applied::compute(&section(dir.path(), |_| {}), now));
    assert!(switched.iter().any(|s| s.kind == Kind::Name && !s.on));
    let dump = format!("{:?}", rt.ring().snapshot());
    assert!(
        !dump.contains(NAME) && !dump.contains("canary-term"),
        "{dump}"
    );
    let (g, purges) = rt.ring().purges_since(0);
    assert_eq!(g, 1);
    assert!(purges[0].kinds.has(Kind::Name) && purges[0].kinds.has(Kind::FilterChange));
    let l = logs(rt);
    let (_, frames) = l.purges_since(0);
    assert_eq!(frames[0].through, rt.id(purges[0].through));

    // A deadline: names on until a moment that passes.
    let timed = section(dir.path(), |l| {
        l.kinds.insert("name".into(), now + 1);
    });
    rt.apply(Applied::compute(&timed, now));
    line(rt, 2, NAME);
    assert!(format!("{:?}", rt.ring().snapshot()).contains(NAME));
    rt.apply(Applied::compute(&timed, now + 2));
    assert!(!format!("{:?}", rt.ring().snapshot()).contains(NAME));
}

#[test]
fn a_deadline_that_passed_hides_at_read_time() {
    let dir = tempfile::tempdir().unwrap();
    let now = unix_now();
    // Applied when the deadline was still ahead; `follow` hasn't
    // re-applied since it passed.
    let l = section(dir.path(), |l| {
        l.kinds.insert("name".into(), now - 1);
    });
    let (rt, _) = runtime(&section(dir.path(), |_| {}), None, true);
    rt.apply(Applied::compute(&l, now - 10));
    line(rt, 1, NAME);
    assert!(
        format!("{:?}", rt.ring().snapshot()).contains(NAME),
        "in the ring"
    );
    assert!(!rt.mask_now().has(Kind::Name));
    let src = logs(rt);
    let page = serde_json::to_string(&src.history(&query("", 100, None)).unwrap()).unwrap();
    assert!(!page.contains(NAME), "{page}");
    let read = src.after(0, &LineFilter::default(), 100);
    assert!(!format!("{:?}", read.lines).contains(NAME));
    // A text filter matching only the hidden value finds nothing.
    let f = LineFilter {
        text: Some("source.fixture".into()),
        ..LineFilter::default()
    };
    assert_eq!(src.after(0, &f, 100).matching, 0);
}

#[test]
fn a_flood_is_its_first_line_and_one_summary() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, console) = runtime(&section(dir.path(), |_| {}), None, true);
    for _ in 0..50 {
        let fields = [("name", Value::Name("x"))];
        rt.record(&Record {
            event: &ev::FLOOD::EVENT,
            fields: &fields,
        });
    }
    // Required events never fold.
    for _ in 0..3 {
        rt.record(&Record {
            event: &ev::NEEDED::EVENT,
            fields: &[],
        });
    }
    rt.flush_folds(true);
    let f = file_text(dir.path());
    let floods: Vec<&str> = f.lines().filter(|l| l.contains("srctest.flood")).collect();
    assert_eq!(floods.len(), 2, "{f}");
    let first: serde_json::Value = serde_json::from_str(floods[0]).unwrap();
    let summary: serde_json::Value = serde_json::from_str(floods[1]).unwrap();
    assert_eq!(summary["repeated"]["count"], 49);
    assert_eq!(summary["repeated"]["seconds"], 10);
    assert_eq!(summary["repeated"]["first"], first["id"]);
    assert_eq!(summary["level"], "warn");
    assert_eq!(summary["fields"], serde_json::json!({}));
    assert_eq!(f.matches("srctest.needed").count(), 3);
    let c = console.text();
    assert_eq!(c.matches("srctest.flood").count(), 2, "{c}");
    assert!(c.contains("flood (repeated 49 times in 10 s)"), "{c}");
    // The ring has the same two.
    let ring: Vec<_> = rt
        .ring()
        .snapshot()
        .into_iter()
        .filter(|e| e.source.name() == "srctest.flood")
        .collect();
    assert_eq!(ring.len(), 2);
    assert_eq!(ring[1].repeated.unwrap().first, Some(ring[0].seq));
}

#[test]
fn a_private_console_folds_what_a_full_file_keeps_apart() {
    let dir = tempfile::tempdir().unwrap();
    let full = section(dir.path(), |l| {
        l.data = DataPreset::Full;
        l.data_until = UNTIL_OFF;
    });
    let (rt, console) = runtime(&full, None, true);
    for name in ["One.Name", "Two.Name", "Three.Name"] {
        let fields = [("name", Value::Name(name))];
        rt.record(&Record {
            event: &ev::FLOOD::EVENT,
            fields: &fields,
        });
    }
    rt.flush_folds(true);
    let f = file_text(dir.path());
    assert_eq!(f.matches("srctest.flood").count(), 3, "{f}");
    assert!(!f.contains("repeated"));
    let c = console.text();
    assert_eq!(c.matches("srctest.flood").count(), 2, "{c}");
    assert!(c.contains("(repeated 2 times in 10 s)"), "{c}");
    assert!(!c.contains("One.Name"), "{c}");
}

#[test]
fn summaries_flush_on_the_timer_and_before_app_stop() {
    let dir = tempfile::tempdir().unwrap();
    let l = section(dir.path(), |_| {});
    let rt = Runtime::with_window(
        Box::new(Buf::default()),
        Applied::compute(&l, unix_now()),
        Duration::from_millis(200),
    );
    let rt: &'static Runtime = Box::leak(Box::new(rt));
    rt.open_file();
    rt.start_timer();
    let flood = |rt: &Runtime| {
        let fields = [("name", Value::Name("x"))];
        rt.record(&Record {
            event: &ev::FLOOD::EVENT,
            fields: &fields,
        });
    };
    flood(rt);
    flood(rt);
    let start = std::time::Instant::now();
    while !file_text(dir.path()).contains("repeated") {
        assert!(start.elapsed() < Duration::from_secs(5), "no summary");
        std::thread::sleep(Duration::from_millis(20));
    }
    // Before the stop line, whatever the window.
    let (rt, _) = runtime(&l, None, true);
    flood(rt);
    flood(rt);
    rt.record(&Record {
        event: &events::APP_STOP::EVENT,
        fields: &[],
    });
    let f = file_text(dir.path());
    let summary = f.rfind("\"repeated\"").unwrap();
    let stop = f.rfind("app.stop").unwrap();
    assert!(summary < stop, "{f}");
}

#[test]
fn history_joins_the_ring_and_the_files_without_gap_or_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let l = section(dir.path(), |l| l.file_size_mib = 1);
    let (rt, _) = runtime(&l, Some(40), true);
    // Small rotations: the cap is a MiB, so rotate by hand as we go.
    for n in 0..300 {
        line(rt, n, "x");
        if n % 70 == 69 {
            if let FileState::Open(sink) = &mut *lock(&rt.file) {
                sink.rotate().unwrap();
            }
        }
    }
    let src = logs(rt);
    let mut got = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page = src.history(&query("hist-line", 23, cursor)).unwrap();
        got.extend(page.lines.iter().map(number));
        pages += 1;
        // Between pages: more lines, another rotation, compression.
        if pages == 2 {
            for n in 300..320 {
                line(rt, n, "x");
            }
            if let FileState::Open(sink) = &mut *lock(&rt.file) {
                sink.rotate().unwrap();
            }
            settle(rt);
        }
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    let want: Vec<u64> = (0..300).rev().collect();
    // The first two pages came before 300..320 were written.
    assert_eq!(got, want, "{} pages", pages);
}

#[test]
fn history_filters_times_limits_and_cursors() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, _) = runtime(&section(dir.path(), |_| {}), Some(10), true);
    for n in 0..30 {
        line(rt, n, "x");
    }
    let src = logs(rt);
    assert_eq!(all_pages(&src, "hist-line", 7).len(), 30);
    let page = src.history(&query("hist-line", 5, None)).unwrap();
    assert_eq!(page.lines.len(), 5);
    assert!(page.more);
    let cursor = page.next_cursor.unwrap();
    // Another filter's cursor, and a bad one.
    assert!(matches!(
        src.history(&query("other", 5, Some(cursor.clone()))),
        Err(LogsError::BadCursor)
    ));
    assert!(matches!(
        src.history(&query("hist-line", 5, Some("junk".into()))),
        Err(LogsError::BadCursor)
    ));
    // Levels and components.
    let mut q = query("", 100, None);
    q.filter.level = Level::Error;
    assert!(src.history(&q).unwrap().lines.is_empty());
    q.filter.level = Level::Trace;
    q.filter.components = Some(vec![Component::Dht]);
    assert!(src.history(&q).unwrap().lines.is_empty());
    // Times: nothing before the epoch of these lines, all after.
    let mut q = query("hist-line", 100, None);
    q.until = Some(0);
    assert!(src.history(&q).unwrap().lines.is_empty());
    q.until = None;
    q.since = Some(1_000);
    assert_eq!(src.history(&q).unwrap().lines.len(), 30);
}

#[test]
fn older_file_lines_show_through_the_mask_now() {
    let dir = tempfile::tempdir().unwrap();
    let full = section(dir.path(), |l| {
        l.data = DataPreset::Full;
        l.data_until = UNTIL_OFF;
    });
    let (rt, _) = runtime(&full, Some(5), true);
    for n in 0..20 {
        line(rt, n, NAME);
    }
    assert!(file_text(dir.path()).contains(NAME), "the file keeps them");
    rt.apply(Applied::compute(&section(dir.path(), |_| {}), unix_now()));
    let src = logs(rt);
    let lines = all_pages(&src, "hist-line", 50);
    assert_eq!(lines.len(), 20);
    let text = serde_json::to_string(&lines).unwrap();
    assert!(!text.contains(NAME), "{text}");
    assert!(text.contains("[hidden]"));
}

#[test]
fn unreadable_lines_are_counted_and_the_cap_gives_partial_pages() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, _) = runtime(&section(dir.path(), |_| {}), Some(1), true);
    let logs_dir = dir.path().join("logs");
    let mut old = String::new();
    for n in 0..2000 {
        old.push_str(&format!(
            r#"{{"ts":"2020-01-01T00:00:00.000Z","level":"info","component":"fetch","event":"srctest.line","msg":"hist-line","fields":{{"n":{{"kind":"size","value":{n}}}}}}}"#
        ));
        old.push('\n');
        if n % 500 == 0 {
            old.push_str("this is not json\n");
            old.push_str(&format!("{}\n", "z".repeat(read::MAX_LINE + 1)));
        }
    }
    std::fs::write(logs_dir.join("lodestar-20200101T000000.000Z.jsonl"), &old).unwrap();
    let src = logs(rt);
    let page = src.history(&query("hist-line", 1000, None)).unwrap();
    assert_eq!(page.lines.len(), 1000);
    let rest = src
        .history(&query("hist-line", 1000, page.next_cursor))
        .unwrap();
    assert_eq!(rest.lines.len(), 1000);
    assert_eq!(page.unreadable + rest.unreadable, 8);
    // A tiny cap: partial pages, never an error, and they add up.
    let mut got = 0;
    let mut cursor = None;
    let mut partial = false;
    loop {
        let p = history::page_capped(
            rt,
            Some(&logs_dir),
            &query("hist-line", 1000, cursor),
            64 << 10,
        )
        .unwrap();
        partial |= p.lines.len() < 1000 && p.more;
        got += p.lines.len();
        match p.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    assert!(partial);
    assert_eq!(got, 2000);
}

#[test]
fn the_seam_holds_while_the_ring_evicts_during_a_walk() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, _) = runtime(&section(dir.path(), |_| {}), Some(300), true);
    for n in 0..2000 {
        line(rt, n, "x");
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let noise = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut i = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                rt.record_untyped(&format::Untyped {
                    level: Level::Info,
                    target: "noise".into(),
                    component: "system",
                    message: format!("noise {i}"),
                    fields: vec![],
                });
                i += 1;
            }
        })
    };
    let src = logs(rt);
    for _ in 0..5 {
        let mut got: Vec<u64> = all_pages(&src, "hist-line", 1000)
            .iter()
            .map(number)
            .collect();
        got.sort_unstable();
        assert_eq!(got, (0..2000).collect::<Vec<u64>>());
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    noise.join().unwrap();
}
