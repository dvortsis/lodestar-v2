//! The `analytics-validation` matrix (architecture §10.17) on the fixed
//! fixture (`lodestar_bench::fixture`), through the real binary: every
//! Insights number the server answers is checked against plain SQL over
//! the fixture's tables, the fixture's own plan, or the Tantivy index, never
//! against the code that serves it. `docs/decisions/2026-11-01-G-m5a.md`
//! maps each §10.17 bullet to this file or to the test that already covers
//! it.
#![cfg(all(unix, feature = "fixture"))]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use lodestar_bench::fixture as data;
use lodestar_pipeline::metric;
use lodestar_store::rusqlite::Connection;
use lodestar_store::{CompactOptions, Store, StoreConfig, flags};
use serde_json::Value;

mod common;
mod fixture_server;

use fixture_server::{PASSWORD, json, request, spawn};

const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

fn u(v: &Value) -> u64 {
    v.as_u64().unwrap_or_else(|| panic!("not a count: {v}"))
}

/// What the fixture's tables hold, read once with plain SQL.
struct Tables {
    /// `(id, discovered_at, metadata_at, imported)`.
    torrents: Vec<(i64, i64, i64, bool)>,
    /// `index_watermark_hour`, by hour: `(hour, last_id)`.
    watermarks: Vec<(i64, i64)>,
}

impl Tables {
    fn read(db: &Path) -> Tables {
        let c = Connection::open(db).unwrap();
        let mut q = c
            .prepare("SELECT id, discovered_at, metadata_at, flags FROM torrents ORDER BY id")
            .unwrap();
        let torrents = q
            .query_map([], |r| {
                let f: i64 = r.get(3)?;
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, f & flags::IMPORTED != 0))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let mut q = c
            .prepare("SELECT hour, last_id FROM index_watermark_hour ORDER BY hour")
            .unwrap();
        let watermarks = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        Tables {
            torrents,
            watermarks,
        }
    }

    /// The hour of the first watermark row covering `id`.
    fn searchable_hour(&self, id: i64) -> Option<i64> {
        let i = self.watermarks.partition_point(|&(_, last)| last < id);
        self.watermarks.get(i).map(|&(h, _)| h)
    }

    fn searchable(&self, id: i64) -> bool {
        self.watermarks.last().is_some_and(|&(_, last)| id <= last)
    }
}

/// An overview or stage page's range: `[from, to)` and its bucket.
struct Range {
    from: i64,
    to: i64,
    width: i64,
}

impl Range {
    fn of(page: &Value) -> Range {
        let r = &page["range"];
        let width = match r["bucket"].as_str() {
            Some("hour") => HOUR,
            Some("day") => DAY,
            b => panic!("bucket {b:?}"),
        };
        Range {
            from: r["from"].as_i64().unwrap(),
            to: r["to"].as_i64().unwrap(),
            width,
        }
    }

    /// The bucket `t` counts in, if the range holds it.
    fn bucket(&self, t: i64) -> Option<i64> {
        (self.from..self.to)
            .contains(&t)
            .then(|| t - t.rem_euclid(self.width))
    }

    /// Hand counts of `times` by bucket.
    fn count(&self, times: impl Iterator<Item = i64>) -> BTreeMap<i64, u64> {
        let mut out = BTreeMap::new();
        for b in times.filter_map(|t| self.bucket(t)) {
            *out.entry(b).or_default() += 1;
        }
        out
    }
}

/// A trend series by bucket; null points must have nothing to count.
fn series(page: &Value, field: &str, want: &BTreeMap<i64, u64>, what: &str) -> u64 {
    let mut sum = 0;
    for p in page["trend"].as_array().unwrap() {
        let at = p["at"].as_i64().unwrap();
        let expected = want.get(&at).copied().unwrap_or(0);
        match p[field].as_u64() {
            Some(n) => assert_eq!(n, expected, "{what} {field} at {at}"),
            None => assert_eq!(expected, 0, "{what} {field} at {at}: null hides counts"),
        }
        sum += expected;
    }
    assert_eq!(
        sum,
        want.values().sum::<u64>(),
        "{what} {field}: every bucket shown"
    );
    sum
}

/// What a crash can lose and `rollup --repair` restores: cohort Stored and
/// Searchable rows, and rollup rows.
type Snapshot = (Vec<(i64, i64, i64, i64)>, Vec<(i64, i64, i64, i64, i64)>);

const STAGES: [&str; 5] = ["harvested", "peer_found", "fetched", "stored", "searchable"];

/// The fixture plan's cohorts, summed into the range's buckets.
fn planned(m: &data::Manifest, r: &Range) -> BTreeMap<i64, BTreeMap<String, i64>> {
    let mut out: BTreeMap<i64, BTreeMap<String, i64>> = BTreeMap::new();
    for c in &m.cohorts {
        let Some(b) = r.bucket(c.cohort * HOUR) else {
            continue;
        };
        let e = out.entry(b).or_default();
        for (k, v) in [
            ("harvested", c.harvested),
            ("peer_found", c.peer_found),
            ("fetched", c.fetched),
            ("in_progress", c.in_progress),
        ] {
            *e.entry(k.into()).or_default() += v;
        }
        for (k, v) in &c.losses {
            *e.entry(format!("lost:{k}")).or_default() += v;
        }
    }
    out
}

#[test]
fn insights_reconcile_on_the_fixture() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("fx");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let anchor = data::anchor(now);
    let m = data::build(&dir, anchor).unwrap();
    // The fixture writes no fetches by event time (metric 14), so the
    // Fetch mark would be 0 = 0: one hand-chosen hour a day for 41 days,
    // the oldest folded below.
    let fetched: Vec<(i64, i64)> = (0..=40)
        .map(|d| (anchor / HOUR - 1 - 24 * d, d + 1))
        .collect();
    {
        let c = Connection::open(dir.join(data::DB)).unwrap();
        for &(h, n) in &fetched {
            c.execute(
                "INSERT INTO rollup_hour (hour, metric, dim, key, value) VALUES (?1, ?2, 3, 0, ?3)",
                lodestar_store::rusqlite::params![h, metric::FUNNEL_EVENTS, n],
            )
            .unwrap();
        }
    }
    // Fold the days past the hourly window (§7), as the recorder's tick
    // does: the all-time range then reads compacted days (30 d starts
    // inside the hourly window).
    let mut store = Store::open(dir.join(data::DB), StoreConfig::default()).unwrap();
    let r = store
        .compact_rollups(CompactOptions {
            now,
            keep_days: 30,
            hold_from_day: None,
            budget: Duration::from_secs(600),
            kind: &metric::aggregation,
        })
        .unwrap();
    assert!(r.done && r.days > 0, "{r:?}");
    drop(store);
    let shown = dir.to_string_lossy().into_owned();
    let mut server = spawn(
        &["--fixture", &shown],
        &[("LODESTAR_FIXTURE_PASSWORD", PASSWORD)],
    );
    server.ready();
    let cookie = server.login();
    let port = server.port;
    let get = |path: &str| json(port, path, &cookie);
    let db = dir.join(data::DB);
    let t = Tables::read(&db);
    let compacted_before: i64 = Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT value FROM insights_state WHERE key = 'compacted_before_day'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    let mut searchable_kpi = None;
    for range in ["24h", "7d", "30d", "all"] {
        // Each page computes its range from its own clock: read them
        // again if an hour turned between the first and the last.
        let read = || {
            let page = |stage: &str| get(&format!("/api/v1/insights/{stage}?range={range}"));
            let o = page("overview");
            let pages = [
                page("discover"),
                page("fetch"),
                page("understand"),
                page("index"),
            ];
            let same = pages.iter().all(|p| p["range"] == o["range"]);
            (o, pages, same)
        };
        let (o, [d, f, un, ix], same) = match read() {
            (.., false) => read(),
            first => first,
        };
        assert!(same, "{range}: a drill-through keeps the range");
        let r = Range::of(&o);
        if range == "all" {
            assert!(
                r.from < compacted_before * DAY,
                "all time reads compacted days"
            );
        }

        // Stored by event time (`metadata_at`), imports apart: the
        // all-time range reads compacted days, so compaction lost nothing.
        let crawled = || t.torrents.iter().filter(|x| !x.3);
        let stored = r.count(crawled().map(|x| x.2));
        let n = series(&un, "stored", &stored, range);
        assert_eq!(u(&un["kpi"]["stored"]), n, "{range}");
        assert_eq!(
            u(&o["stages"]["understand"]),
            n,
            "{range}: the Understand mark"
        );

        // Newly searchable by event time: the first watermark hour
        // covering each id.
        let by_hour = |imported: bool| {
            r.count(
                t.torrents
                    .iter()
                    .filter(|x| x.3 == imported)
                    .filter_map(|x| t.searchable_hour(x.0))
                    .map(|h| h * HOUR),
            )
        };
        let (new, imp) = (by_hour(false), by_hour(true));
        let n = series(&o, "searchable", &new, range);
        let n_imp = series(&o, "searchable_imported", &imp, range);
        assert_eq!(series(&ix, "new_searchable", &new, range), n);
        assert_eq!(series(&ix, "new_searchable_imported", &imp, range), n_imp);
        for p in [&o, &ix] {
            assert_eq!(u(&p["kpi"]["new_searchable"]), n, "{range}");
            assert_eq!(u(&p["kpi"]["new_searchable_imported"]), n_imp, "{range}");
        }

        // The funnel by cohort: stored and searchable from `torrents` by
        // `discovered_at`; the rest from the fixture's plan.
        let by_cohort =
            |f: &dyn Fn(i64) -> bool| r.count(crawled().filter(|x| f(x.0)).map(|x| x.1));
        let stored = by_cohort(&|_| true);
        let searchable = by_cohort(&|id| t.searchable(id));
        let plan = planned(&m, &r);
        let funnel = o["funnel"].as_array().unwrap();
        let mut total: BTreeMap<&str, u64> = BTreeMap::new();
        for e in funnel {
            let c = e["cohort"].as_i64().unwrap();
            let st = |k: &str| u(&e["stages"][k]);
            assert_eq!(
                st("stored"),
                stored.get(&c).copied().unwrap_or(0),
                "{range} {c}"
            );
            assert_eq!(
                st("searchable"),
                searchable.get(&c).copied().unwrap_or(0),
                "{range} {c}"
            );
            let want = plan.get(&c).cloned().unwrap_or_default();
            let p = |k: &str| want.get(k).copied().unwrap_or(0) as u64;
            for k in ["harvested", "peer_found", "fetched", "in_progress"] {
                let got = if k == "in_progress" { u(&e[k]) } else { st(k) };
                assert_eq!(got, p(k), "{range} {c} {k}");
            }
            let mut lost = 0;
            for (k, v) in e["losses"].as_object().unwrap() {
                assert_eq!(u(v), p(&format!("lost:{k}")), "{range} {c} {k}");
                lost += u(v);
            }
            // Funnels never increase; harvested = terminal + in progress.
            for w in STAGES.windows(2) {
                assert!(st(w[0]) >= st(w[1]), "{range} {c}: {} < {}", w[0], w[1]);
            }
            assert_eq!(
                st("harvested"),
                lost + st("stored") + u(&e["in_progress"]),
                "{range} {c}"
            );
            for k in STAGES {
                *total.entry(k).or_default() += st(k);
            }
        }
        let shown: Vec<i64> = funnel
            .iter()
            .map(|e| e["cohort"].as_i64().unwrap())
            .collect();
        for c in stored.keys().chain(plan.keys()) {
            assert!(
                shown.contains(c),
                "{range}: cohort {c} has counts but isn't listed"
            );
        }
        for k in STAGES {
            assert_eq!(u(&o["funnel_total"]["stages"][k]), total[k], "{range} {k}");
        }

        // Each stage mark's drill-through lands on a page with its count.
        assert_eq!(
            f["funnel"], o["funnel"],
            "{range}: Fetch's funnel is the overview's"
        );
        assert_eq!(f["funnel_total"], o["funnel_total"], "{range}");
        assert_eq!(u(&o["stages"]["discover"]), total["harvested"], "{range}");
        assert_eq!(u(&d["kpi"]["harvested"]), total["harvested"], "{range}");
        let want = r.count(
            fetched
                .iter()
                .flat_map(|&(h, n)| std::iter::repeat_n(h * HOUR, n as usize)),
        );
        let n = series(&f, "fetched", &want, range);
        assert!(n > 0, "{range}: something was fetched");
        assert_eq!(u(&f["kpi"]["fetched"]), n, "{range}");
        assert_eq!(u(&o["stages"]["fetch"]), n, "{range}: the Fetch mark");
        assert_eq!(o["stages"]["index"], ix["kpi"]["searchable"], "{range}");
        assert_eq!(o["kpi"]["searchable"], ix["kpi"]["searchable"], "{range}");
        searchable_kpi = Some(u(&o["kpi"]["searchable"]));
    }

    // "Searchable torrents" is the index's document count at its last
    // commit: as `/metrics` reports it, and as Tantivy counts it.
    let searchable = searchable_kpi.unwrap();
    let metrics = request(port, "GET /metrics HTTP/1.0", "").unwrap();
    assert!(
        metrics.contains(&format!("\nlodestar_search_docs {searchable}\n")),
        "{metrics}"
    );
    let all_ids = t.torrents.iter().filter(|x| t.searchable(x.0)).count() as u64;
    assert_eq!(
        searchable, all_ids,
        "every id under the watermark is a document"
    );
    server.stop();
    let index = lodestar_search::SearchIndex::open(&lodestar_search::index_dir(&dir.join("index")))
        .unwrap();
    assert_eq!(index.num_docs(), searchable, "Tantivy's count");
    drop(index);

    // `rollup --repair` after a simulated crash: the last two days' stored
    // and searchable rows never landed. The repair restores them exactly.
    let since = data::anchor(now) / HOUR - 48;
    let snapshot = |c: &Connection| -> Snapshot {
        let cohorts = c
            .prepare(
                "SELECT cohort, source, stage, count FROM cohort_hour
                 WHERE stage IN (4, 5) ORDER BY 1, 2, 3",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        // The delay histogram (11) is recounted at each watermark hour's
        // midpoint (`repair_rollups`), so only its per-hour total is exact.
        let rollups = c
            .prepare(
                "SELECT hour, metric, dim, key, value FROM rollup_hour WHERE metric IN (4, 10)
                 UNION ALL
                 SELECT hour, 11, 0, 0, sum(value) FROM rollup_hour WHERE metric = 11 GROUP BY hour
                 ORDER BY 1, 2, 3, 4",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        (cohorts, rollups)
    };
    let c = Connection::open(&db).unwrap();
    let good = snapshot(&c);
    c.execute_batch(&format!(
        "DELETE FROM cohort_hour WHERE cohort >= {since} AND stage IN (4, 5);
         DELETE FROM rollup_hour WHERE hour >= {since} AND metric IN (4, 10, 11);"
    ))
    .unwrap();
    assert_ne!(snapshot(&c), good, "the crash lost rows");
    drop(c);
    let out = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .args(["rollup", "--repair"])
        .env("LODESTAR_DATA_DIR", &dir)
        .env("LODESTAR_LOG", "warn")
        .env("PUID", "0")
        .env("PGID", "0")
        .env("LODESTAR_ALLOW_ROOT", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        snapshot(&Connection::open(&db).unwrap()),
        good,
        "repaired exactly"
    );
}
