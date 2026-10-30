//! `/logs` over a real runtime: the ring and its ids, purges, folding,
//! history's join with the files, the files, downloads, the archive and
//! deletion.

use std::sync::Arc;
use std::time::Duration;

use lodestar_api::logs::{
    HistoryQuery, LineFilter, LogFilesWhich, LogFormat, LogLine, LogSource, LogStreamConfig,
    LogValue, LogsError,
};
use lodestar_log::{Record, Sink, Value};

use super::names::LogName;
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
    assert!(
        console.text().contains("setup token TOKEN-ABCD-1234"),
        "the console has it, right after the message"
    );
    assert!(
        file_text(dir.path()).contains("TOKEN-ABCD-1234"),
        "the file has it"
    );
    let dump = format!("{:?}", rt.ring().snapshot());
    assert!(!dump.contains("TOKEN-"), "{dump}");
    let l = logs(rt);
    let page = l.history(&query("", 1000, None)).unwrap();
    let text = serde_json::to_string(&page).unwrap();
    assert!(!text.contains("TOKEN-"), "{text}");
    assert!(text.contains("[redacted]"));
    // An older file's plain start-up line (before B-log1b typed it).
    let file = dir.path().join("logs").join(file::CURRENT);
    let old = r#"{"ts":"2026-10-01T00:00:00.000Z","level":"warn","component":"system","target":"lodestar::services","msg":"first run: open x and enter setup token TOKEN-EFGH-5678 to create","fields":{}}"#;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap();
    std::io::Write::write_all(&mut f, format!("{old}\n").as_bytes()).unwrap();
    drop(f);
    // Past the ring: from the file, both redacted.
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
        !text.contains("TOKEN-") && text.matches("[redacted]").count() >= 2,
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
            // No settle: compression races the paging, as in life.
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

#[test]
fn file_lines_are_cut_as_the_ring_cuts_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, _) = runtime(&section(dir.path(), |_| {}), Some(1), true);
    let big = "w".repeat(200_000);
    std::fs::write(
        dir.path().join("logs").join("lodestar-20200101T000000.000Z.jsonl"),
        format!(
            "{{\"ts\":\"2020-01-01T00:00:00.000Z\",\"level\":\"info\",\"component\":\"fetch\",\"target\":\"t\",\"msg\":\"hist-line {big}\",\"fields\":{{\"v\":{{\"kind\":\"untyped\",\"value\":\"{big}\"}}}}}}\n"
        ),
    )
    .unwrap();
    let page = logs(rt).history(&query("hist-line", 10, None)).unwrap();
    assert_eq!(page.lines.len(), 1);
    let l = &page.lines[0];
    assert!(l.truncated);
    assert!(serde_json::to_string(l).unwrap().len() < 17_000);
}

#[test]
fn the_file_list_downloads_and_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    let now = unix_now();
    let names = section(dir.path(), |l| {
        l.kinds.insert("name".into(), now + 600);
    });
    let (rt, _) = runtime(&names, None, false);
    line(rt, 1, NAME);
    if let FileState::Open(sink) = &mut *lock(&rt.file) {
        sink.rotate().unwrap();
    }
    settle(rt);
    rt.apply(Applied::compute(&section(dir.path(), |_| {}), now));
    line(rt, 2, "plain");
    let src = logs(rt);
    let listed = src.files();
    assert_eq!(listed.files.len(), 2, "{listed:?}");
    assert!(listed.index_readable);
    let (current, rotated) = (&listed.files[0], &listed.files[1]);
    assert!(current.current && !current.details && !current.compressed);
    assert!(!rotated.current && rotated.details && rotated.compressed);
    assert!(rotated.from.is_some() && rotated.to.is_some());
    assert!(current.to.is_some());
    let read = |d: lodestar_api::logs::Download| -> String {
        String::from_utf8(
            d.chunks
                .collect::<std::io::Result<Vec<_>>>()
                .unwrap()
                .concat(),
        )
        .unwrap()
    };
    // Redacted by default when it has details.
    let d = src.download(&rotated.name, LogFormat::Jsonl, None).unwrap();
    assert!(d.file_name.ends_with("-redacted.jsonl"), "{}", d.file_name);
    assert_eq!(d.content_type, "application/x-ndjson");
    let text = read(d);
    assert!(
        !text.contains(NAME) && text.contains("srctest.line"),
        "{text}"
    );
    let raw = read(
        src.download(&rotated.name, LogFormat::Jsonl, Some(false))
            .unwrap(),
    );
    assert!(raw.contains(NAME));
    let txt = src.download(&current.name, LogFormat::Text, None).unwrap();
    assert_eq!(txt.file_name, "lodestar.txt");
    assert!(read(txt).contains("hist-line n=2 name="));
    for bad in [
        "../lodestar.jsonl",
        "index.json",
        "lodestar-20990101T000000.000Z.jsonl",
    ] {
        assert!(matches!(
            src.download(bad, LogFormat::Jsonl, None),
            Err(LogsError::NotFound)
        ));
    }
    // The archive: every file, redacted by default since one has details.
    let a = src.archive(LogFormat::Jsonl, None).unwrap();
    assert!(a.file_name.ends_with("-redacted.zip"));
    let zip_bytes = a
        .chunks
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap()
        .concat();
    let members = zip::read(&zip_bytes);
    assert_eq!(members.len(), 2);
    for (name, body) in &members {
        let body = String::from_utf8_lossy(body);
        assert!(!body.contains(NAME), "{name}: {body}");
        assert!(name.ends_with("-redacted.jsonl"), "{name}");
    }
    // An unreadable index: every file has details.
    std::fs::write(dir.path().join("logs").join(file::INDEX), "{oops").unwrap();
    let listed = src.files();
    assert!(!listed.index_readable);
    assert!(listed.files.iter().all(|f| f.details));
}

#[cfg(unix)]
#[test]
fn a_link_named_like_a_log_is_refused_and_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, _) = runtime(&section(dir.path(), |_| {}), None, false);
    line(rt, 1, "x");
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, "do not read").unwrap();
    let name = "lodestar-20200101T000000.000Z.jsonl";
    std::os::unix::fs::symlink(&secret, dir.path().join("logs").join(name)).unwrap();
    let src = logs(rt);
    assert!(!src.files().files.iter().any(|f| f.name == name));
    assert!(matches!(
        src.download(name, LogFormat::Jsonl, Some(false)),
        Err(LogsError::NotFound)
    ));
    let done = src.delete(LogFilesWhich::All).unwrap();
    assert_eq!(done.skipped.len(), 1);
    assert_eq!(done.skipped[0].name, name);
    assert_eq!(done.skipped[0].reason, "not_a_regular_file");
    assert!(secret.exists(), "never followed");
}

#[test]
fn delete_while_writers_run_and_a_compression_is_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    // No prune by count: the writer rotates every 300 lines, far faster
    // than the worker compresses, so a prune queued ahead of the deletion
    // would see the newer files and take the older ones first, leaving
    // the deletion nothing (`a_prune_queued_ahead_can_take_what_a_deletion_names`).
    let (rt, _) = runtime(
        &section(dir.path(), |l| l.file_count = u32::MAX),
        None,
        false,
    );
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let lines = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let writer = {
        let stop = stop.clone();
        let lines = lines.clone();
        std::thread::spawn(move || {
            let mut n = 0;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                line(rt, n, "x");
                if n % 300 == 299 {
                    if let FileState::Open(sink) = &mut *lock(&rt.file) {
                        let _ = sink.rotate();
                    }
                }
                n += 1;
                lines.store(n, std::sync::atomic::Ordering::Relaxed);
            }
            n
        })
    };
    // Wait for two rotations rather than a fixed time: a slow runner may
    // not have written 300 lines in any set sleep.
    // A loaded machine (the whole workspace's tests at once) can need more
    // than 10 s: without the rotations nothing is there to delete.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while lines.load(std::sync::atomic::Ordering::Relaxed) < 600
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let seen = lines.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        seen >= 600,
        "the writer didn't rotate twice within 30 s: {seen} of 600 lines"
    );
    let src = logs(rt);
    let done = src.delete(LogFilesWhich::All).unwrap();
    assert!(done.deleted >= 1, "{done:?}");
    // Writers go on after it: two more rotations' worth of lines.
    let after = lines.load(std::sync::atomic::Ordering::Relaxed) + 600;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while lines.load(std::sync::atomic::Ordering::Relaxed) < after
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let seen = lines.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        seen >= after,
        "the writer didn't write 600 more lines within 30 s: {seen} of {after} lines"
    );
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let written = writer.join().unwrap();
    settle(rt);
    // What's left in the folder is one unbroken run up to the last line
    // written: no line lost from the new files, none from before brought
    // back. (The ring isn't touched by a deletion, so it's read here, not
    // history.)
    let mut left = folder_numbers(&dir.path().join("logs"));
    left.sort_unstable();
    assert!(!left.is_empty());
    let first = left[0];
    assert!(first > 0, "something was deleted");
    assert_eq!(left, (first..written).collect::<Vec<u64>>());
    // The index names only files that exist.
    let raw: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("logs").join(file::INDEX))
            .unwrap_or_else(|_| "{}".into()),
    )
    .unwrap();
    if let Some(files) = raw["files"].as_object() {
        for name in files.keys() {
            assert!(dir.path().join("logs").join(name).exists(), "{name}");
        }
    }
}

/// A deletion queued behind a compression whose prune sees newer files
/// (rotated meanwhile) finds what it named already pruned by count: it
/// reports nothing deleted, skips nothing, and leaves nothing at or older
/// than the file it set aside. With no prune by count it takes both. It
/// shows the delete test's old flake; the prune's own order (a newer
/// `-N` kept over an older name) is `the_count_keeps_the_newest_of_one_millisecond`.
#[test]
fn a_prune_queued_ahead_can_take_what_a_deletion_names() {
    fn run(count: u32) -> (lodestar_api::logs::LogFilesDeleted, Vec<String>, String) {
        let dir = tempfile::tempdir().unwrap();
        let logs_dir = dir.path().join("logs");
        let (rt, _) = runtime(&section(dir.path(), |l| l.file_count = count), None, false);
        let rotate = |n: u64| {
            line(rt, n, "x");
            match &mut *lock(&rt.file) {
                FileState::Open(sink) => sink.rotate().unwrap(),
                _ => panic!("the file isn't open"),
            }
        };
        settle(rt);
        let hold = match &*lock(&rt.file) {
            FileState::Open(sink) => sink.hold(),
            _ => panic!("the file isn't open"),
        };
        // Its compression and prune queue behind the hold.
        let first = rotate(0);
        line(rt, 1, "x");
        let deleting = std::thread::spawn(move || logs(rt).delete(LogFilesWhich::All).unwrap());
        // The deletion has set the current file aside once a second
        // rotated file is there.
        let rotated = || -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(&logs_dir)
                .unwrap()
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter(|n| n.starts_with("lodestar-"))
                .collect();
            v.sort();
            v
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let aside = loop {
            if let Some(n) = rotated().into_iter().find(|n| *n != first) {
                break n;
            }
            assert!(std::time::Instant::now() < deadline, "{:?}", rotated());
            std::thread::sleep(Duration::from_millis(5));
        };
        for n in 2..5 {
            rotate(n);
        }
        drop(hold);
        let done = deleting.join().unwrap();
        settle(rt);
        (done, rotated(), aside)
    }
    let newer = |a: &str, b: &str| {
        let (a, b) = (LogName::parse(a).unwrap(), LogName::parse(b).unwrap());
        LogName::newest_first(&a, &b) == std::cmp::Ordering::Less
    };
    // Pruned to 2 by the first file's prune, before the deletion runs.
    let (done, left, aside) = run(2);
    assert_eq!(done.deleted, 0, "{done:?}");
    assert!(done.skipped.is_empty(), "{done:?}");
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(left.iter().all(|n| newer(n, &aside)), "{left:?} {aside}");
    // Nothing pruned: the deletion takes the first file and the one set aside.
    let (done, left, aside) = run(u32::MAX);
    assert_eq!(done.deleted, 2, "{done:?}");
    assert_eq!(left.len(), 3, "{left:?}");
    assert!(left.iter().all(|n| newer(n, &aside)), "{left:?} {aside}");
}

/// Every `srctest.line` number in the folder's files.
fn folder_numbers(dir: &std::path::Path) -> Vec<u64> {
    let mut out = Vec::new();
    for l in files::list(dir).unwrap() {
        let mut r = files::open_plain(dir, &l.name).unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut r, &mut text).unwrap();
        for line in text.lines().filter(|l| l.contains("srctest.line")) {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            out.push(v["fields"]["n"]["value"].as_u64().unwrap());
        }
    }
    out
}

#[test]
fn delete_details_only() {
    let dir = tempfile::tempdir().unwrap();
    let now = unix_now();
    let (rt, _) = runtime(
        &section(dir.path(), |l| {
            l.kinds.insert("name".into(), now + 600);
        }),
        None,
        false,
    );
    line(rt, 1, NAME);
    if let FileState::Open(sink) = &mut *lock(&rt.file) {
        sink.rotate().unwrap();
    }
    rt.apply(Applied::compute(&section(dir.path(), |_| {}), now));
    line(rt, 2, "plain");
    if let FileState::Open(sink) = &mut *lock(&rt.file) {
        sink.rotate().unwrap();
    }
    settle(rt);
    let src = logs(rt);
    assert_eq!(src.files().files.len(), 3);
    let deleted_before = events::LOGGING_FILES_DELETED::EVENT.count();
    let done = src.delete(LogFilesWhich::Details).unwrap();
    assert_eq!(done.deleted, 1, "{done:?}");
    let left = src.files();
    assert!(left.files.iter().all(|f| !f.details), "{left:?}");
    let all = serde_json::to_string(&all_pages(&src, "hist-line", 100)).unwrap();
    assert!(!all.contains(NAME));
    // The deletion is logged (tests have no global sink: its counter).
    assert!(events::LOGGING_FILES_DELETED::EVENT.count() > deleted_before);
}

/// The moment a badge's file has its new name and the index on disk still
/// has the old one (a compression between a listing and the index read):
/// the file still counts as having details.
#[test]
fn a_badge_holds_while_its_file_changes_name() {
    let dir = tempfile::tempdir().unwrap();
    let now = unix_now();
    let (rt, _) = runtime(
        &section(dir.path(), |l| {
            l.kinds.insert("name".into(), now + 600);
        }),
        None,
        false,
    );
    line(rt, 1, NAME);
    if let FileState::Open(sink) = &mut *lock(&rt.file) {
        sink.rotate().unwrap();
    }
    settle(rt);
    rt.apply(Applied::compute(&section(dir.path(), |_| {}), now));
    line(rt, 2, "plain");
    let gz = std::fs::read_dir(dir.path().join("logs"))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .find(|n| n.ends_with(".jsonl.gz"))
        .expect("the compressed file");
    let plain = gz.trim_end_matches(".gz").to_string();
    // The index names it by its `.jsonl` only.
    let index = lock(&rt.jobs).clone().unwrap().index;
    {
        let mut ix = file::lock(&index);
        let f = ix.files.remove(&gz).expect("its badge");
        ix.files.insert(plain.clone(), f);
        ix.remove("nothing-to-remove").unwrap();
    }
    let raw = std::fs::read_to_string(dir.path().join("logs").join(file::INDEX)).unwrap();
    std::fs::write(
        dir.path().join("logs").join(file::INDEX),
        raw.replace(&format!("\"{gz}\""), &format!("\"{plain}\"")),
    )
    .unwrap();
    let src = logs(rt);
    let listed = src.files();
    let f = listed.files.iter().find(|f| f.name == gz).unwrap();
    assert!(f.details, "{listed:?}");
    let d = src.download(&gz, LogFormat::Jsonl, None).unwrap();
    assert!(d.file_name.ends_with("-redacted.jsonl"));
    assert!(
        src.archive(LogFormat::Jsonl, None)
            .unwrap()
            .file_name
            .ends_with("-redacted.zip")
    );
    // "Details" picks it in the job, by the writer's index.
    let done = src.delete(LogFilesWhich::Details).unwrap();
    assert_eq!(done.deleted, 1, "{done:?}");
    assert!(!dir.path().join("logs").join(&gz).exists());
}

#[cfg(windows)]
#[test]
fn delete_and_rotate_while_a_download_is_open() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, _) = runtime(&section(dir.path(), |_| {}), None, false);
    for n in 0..1000 {
        line(rt, n, "x");
    }
    let src = logs(rt);
    let mut d = src
        .download("lodestar.jsonl", LogFormat::Jsonl, Some(false))
        .unwrap();
    let first = d.chunks.next().unwrap().unwrap();
    assert!(!first.is_empty());
    if let FileState::Open(sink) = &mut *lock(&rt.file) {
        sink.rotate().expect("rotation with a reader open");
    }
    let done = src.delete(LogFilesWhich::All).unwrap();
    assert!(done.skipped.is_empty(), "{done:?}");
    drop(d);
}
