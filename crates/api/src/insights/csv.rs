//! `GET /insights/export.csv?panel=` (§10.9.6: every chart has a CSV
//! export).
//!
//! - **The same numbers as the chart:** the rows are the JSON the panel's
//!   own endpoint serves (the same builder, the same cache entry), turned
//!   into CSV, so the two can't disagree.
//! - **RFC 4180:** `\r\n` line ends and a header of field names. Nested
//!   objects become `parent.child` columns. Values are raw: numbers as
//!   numbers, ratios 0–1, times as Unix seconds with an ISO-8601 `…_utc`
//!   column beside them. **Null is an empty cell, never 0.**
//! - **Spreadsheet injection:** every text cell goes through
//!   [`crate::export::csv_cell`] (a `'` before a leading `=`, `+`, `-`,
//!   `@`, tab or CR; controls and bidi formatting written `\u{…}`; quoted
//!   when it holds a comma or a quote). Numbers are written from typed
//!   numbers and never escaped, so `-3` stays a number.
//! - **The file name** is built only from the validated panel and the
//!   range's integers.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::{ApiError, ApiResult, ApiState, OverviewQuery, Stage, overview_response};
use crate::dto::insights::{ExportQuery, StageQuery};
use crate::export::csv_cell;
use crate::extract::Query;

/// Where a panel's rows are in its endpoint's JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Panel {
    name: &'static str,
    /// `None`: the overview.
    stage: Option<Stage>,
    /// A JSON pointer to the rows.
    rows: &'static str,
    /// Where the previous period's rows are, if it has them.
    previous: Option<&'static str>,
    /// The columns, so a panel with no rows still has its header; a key a
    /// row has beyond them (an unknown loss reason) is added.
    columns: &'static [&'static str],
}

const fn overview(
    name: &'static str,
    rows: &'static str,
    previous: Option<&'static str>,
    columns: &'static [&'static str],
) -> Panel {
    Panel {
        name,
        stage: None,
        rows,
        previous,
        columns,
    }
}

const fn stage(
    name: &'static str,
    stage: Stage,
    rows: &'static str,
    previous: Option<&'static str>,
    columns: &'static [&'static str],
) -> Panel {
    Panel {
        name,
        stage: Some(stage),
        rows,
        previous,
        columns,
    }
}

const FUNNEL: &[&str] = &[
    "cohort",
    "conversion.fetched",
    "conversion.peer_found",
    "conversion.searchable",
    "conversion.stored",
    "final",
    "in_progress",
    "losses.blocked",
    "losses.encryption_failed",
    "losses.evicted",
    "losses.gave_up",
    "losses.invalid",
    "losses.no_peers",
    "losses.oversize",
    "losses.refused",
    "losses.timed_out",
    "losses.unreachable",
    "stages.fetched",
    "stages.harvested",
    "stages.peer_found",
    "stages.searchable",
    "stages.stored",
];
const COUNTS: &[&str] = &["key", "searchable"];

/// Every panel with rows. Fields that are null until their counter
/// exists (`by_ip_family`, `trackers`, `top_queries`) have none yet.
const PANELS: [Panel; 20] = [
    overview(
        "trend",
        "/trend",
        Some("/previous/trend"),
        &["at", "harvested", "searchable", "searchable_imported"],
    ),
    overview("funnel", "/funnel", None, FUNNEL),
    // Built from `funnel_total.losses` and `.loss_share`.
    overview(
        "losses",
        "/funnel_total",
        None,
        &["count", "reason", "share"],
    ),
    overview(
        "segments.by_group",
        "/segments/by_group",
        Some("/previous/segments/by_group"),
        COUNTS,
    ),
    overview(
        "segments.by_content_type",
        "/segments/by_content_type",
        Some("/previous/segments/by_content_type"),
        COUNTS,
    ),
    overview(
        "segments.by_source",
        "/segments/by_source",
        Some("/previous/segments/by_source"),
        &["harvested", "key", "searchable", "yield"],
    ),
    overview(
        "exceptions",
        "/exceptions",
        None,
        &[
            "fix.kind",
            "fix.target",
            "kind",
            "severity",
            "since",
            "unit",
            "value",
        ],
    ),
    overview(
        "annotations",
        "/annotations",
        None,
        // `at,kind,text` first, as before IMP-5, so positional readers
        // keep working; the import's fields follow.
        &[
            "at",
            "kind",
            "text",
            "import.job",
            "import.merged",
            "import.new",
            "import.removed",
            "import.source",
        ],
    ),
    stage(
        "discover.trend",
        Stage::Discover,
        "/trend",
        Some("/previous/trend"),
        &["at", "dropped", "harvested", "rediscovered", "reported"],
    ),
    stage(
        "discover.by_source",
        Stage::Discover,
        "/by_source",
        None,
        &["dropped", "key", "rediscovered", "reported"],
    ),
    stage(
        "discover.by_strategy",
        Stage::Discover,
        "/by_strategy",
        None,
        &["key", "reported"],
    ),
    stage(
        "fetch.trend",
        Stage::Fetch,
        "/trend",
        Some("/previous/trend"),
        &["at", "fetched", "lost"],
    ),
    stage(
        "fetch.losses",
        Stage::Fetch,
        "/losses",
        None,
        &["count", "key", "share"],
    ),
    stage("fetch.funnel", Stage::Fetch, "/funnel", None, FUNNEL),
    stage(
        "understand.trend",
        Stage::Understand,
        "/trend",
        Some("/previous/trend"),
        &["at", "blocked", "stored"],
    ),
    stage(
        "understand.by_content_type",
        Stage::Understand,
        "/by_content_type",
        None,
        COUNTS,
    ),
    stage(
        "understand.by_group",
        Stage::Understand,
        "/by_group",
        None,
        COUNTS,
    ),
    stage(
        "index.trend",
        Stage::Index,
        "/trend",
        Some("/previous/trend"),
        &[
            "at",
            "new_searchable",
            "new_searchable_imported",
            "removed",
            "searchable",
        ],
    ),
    stage(
        "index.removed_by_type",
        Stage::Index,
        "/removed_by_type",
        None,
        &["key", "removed"],
    ),
    stage(
        "serve.trend",
        Stage::Serve,
        "/trend",
        Some("/previous/trend"),
        &["at", "searches", "zero_results"],
    ),
];

fn panel(name: Option<&str>) -> ApiResult<Panel> {
    let bad = || {
        ApiError::bad_request(
            "bad_parameter",
            "Unknown panel: use trend, funnel, losses, segments.by_group, \
             segments.by_content_type, segments.by_source, exceptions, annotations, \
             or a stage's panel such as fetch.trend.",
        )
        .field("panel")
    };
    let name = name.ok_or_else(bad)?;
    PANELS
        .iter()
        .find(|p| p.name == name)
        .copied()
        .ok_or_else(bad)
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian (H.
/// Hinnant's `civil_from_days`).
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
fn iso_utc(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
}

/// Columns holding Unix seconds: each gets an `…_utc` column beside it.
const TIMES: [&str; 4] = ["at", "cohort", "since", "to"];

/// `row`'s fields as `name → value`, nested objects flattened to
/// `parent.child`.
fn flatten(prefix: &str, v: &Value, out: &mut BTreeMap<String, Value>) {
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&key, v, out);
            }
        }
        other => {
            out.insert(prefix.to_string(), other.clone());
        }
    }
}

fn cell(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        // From a typed number: never escaped.
        Value::Number(n) => n.to_string(),
        Value::String(s) => csv_cell(s),
        other => csv_cell(&other.to_string()),
    }
}

/// The panel's rows as `(period, fields)`.
fn rows(p: &Panel, v: &Value, compare: bool) -> Vec<(&'static str, BTreeMap<String, Value>)> {
    let mut out = Vec::new();
    let mut take = |period: &'static str, at: &str| {
        let Some(found) = v.pointer(at) else { return };
        if p.name == "losses" {
            // One row per reason: its count and its share of all losses.
            let losses = found.get("losses").and_then(Value::as_object);
            let shares = found.get("loss_share").and_then(Value::as_object);
            for (reason, n) in losses.into_iter().flatten() {
                let mut row = BTreeMap::new();
                row.insert("count".into(), n.clone());
                row.insert("reason".into(), Value::String(reason.clone()));
                row.insert(
                    "share".into(),
                    shares
                        .and_then(|s| s.get(reason))
                        .cloned()
                        .unwrap_or(Value::Null),
                );
                out.push((period, row));
            }
            return;
        }
        for item in found.as_array().into_iter().flatten() {
            let mut row = BTreeMap::new();
            flatten("", item, &mut row);
            // A null object the panel declares by its fields (an
            // annotation's `import`) is those fields' empty cells, not a
            // column of its own.
            row.retain(|k, v| {
                !(v.is_null()
                    && p.columns.iter().any(|c| {
                        c.strip_prefix(k.as_str())
                            .is_some_and(|r| r.starts_with('.'))
                    }))
            });
            out.push((period, row));
        }
    };
    take("current", p.rows);
    if let (true, Some(prev)) = (compare, p.previous) {
        take("previous", prev);
    }
    out
}

/// The CSV text: a header of field names (`period` first, then the
/// panel's columns in its order, then any other field a row has, sorted;
/// each time column followed by its `…_utc`), then one line per row.
fn render(columns: &[&str], rows: &[(&'static str, BTreeMap<String, Value>)]) -> String {
    let mut fields: Vec<String> = columns.iter().map(|c| c.to_string()).collect();
    let mut extra: Vec<String> = Vec::new();
    for (_, r) in rows {
        for k in r.keys() {
            if !fields.contains(k) && !extra.contains(k) {
                extra.push(k.clone());
            }
        }
    }
    extra.sort();
    fields.extend(extra);
    let mut columns = vec!["period".to_string()];
    for f in &fields {
        columns.push(f.clone());
        if TIMES.contains(&f.as_str()) {
            columns.push(format!("{f}_utc"));
        }
    }
    let mut out = columns.join(",");
    out.push_str("\r\n");
    for (period, r) in rows {
        let mut line = vec![(*period).to_string()];
        for f in &fields {
            let v = r.get(f).unwrap_or(&Value::Null);
            line.push(cell(v));
            if TIMES.contains(&f.as_str()) {
                line.push(v.as_i64().map(iso_utc).unwrap_or_default());
            }
        }
        out.push_str(&line.join(","));
        out.push_str("\r\n");
    }
    out
}

/// One panel of Insights as CSV, the numbers its chart shows.
#[utoipa::path(
    operation_id = "getInsightsExportCsv",
    get,
    path = "/insights/export.csv",
    tag = "insights",
    params(ExportQuery),
    responses((
        status = 200,
        description = "The panel's rows as an attachment \
            (`lodestar-insights-<panel>-<from>-<to>.csv`).",
        content_type = "text/csv",
        body = String,
    ))
)]
pub(crate) async fn export_csv(
    State(s): State<ApiState>,
    Query(q): Query<ExportQuery>,
) -> ApiResult<Response> {
    let p = panel(q.panel.as_deref())?;
    super::stages::reason_code(q.reason.as_deref())?;
    let compare = q.compare == Some(1);
    let value = match p.stage {
        None => serde_json::to_value(
            overview_response(
                &s,
                OverviewQuery {
                    range: q.range,
                    from: q.from,
                    to: q.to,
                    compare: q.compare,
                },
            )
            .await?,
        )
        .map_err(crate::error::internal)?,
        Some(stage) => super::stage_response(
            &s,
            stage,
            StageQuery {
                range: q.range,
                from: q.from,
                to: q.to,
                compare: q.compare,
                reason: q.reason,
            },
        )
        .await?
        .to_value(),
    };
    let body = render(p.columns, &rows(&p, &value, compare));
    let int = |at: &str| value.pointer(at).and_then(Value::as_i64).unwrap_or(0);
    // Only the validated panel name and integers.
    let file = format!(
        "attachment; filename=\"lodestar-insights-{}-{}-{}.csv\"",
        p.name,
        int("/range/from"),
        int("/range/to")
    );
    Ok((
        [
            (
                header::CONTENT_TYPE,
                "text/csv; charset=utf-8; header=present".to_string(),
            ),
            (header::CONTENT_DISPOSITION, file),
            (header::CACHE_CONTROL, "no-store".to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        body,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    /// An RFC 4180 reader for the tests: records of fields.
    fn parse(text: &str) -> Vec<Vec<String>> {
        let mut records = Vec::new();
        let mut record = Vec::new();
        let mut field = String::new();
        let mut quoted = false;
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match (quoted, c) {
                (true, '"') if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                (true, '"') => quoted = false,
                (true, c) => field.push(c),
                (false, '"') if field.is_empty() => quoted = true,
                (false, ',') => record.push(std::mem::take(&mut field)),
                (false, '\r') if chars.peek() == Some(&'\n') => {
                    chars.next();
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                }
                (false, c) => field.push(c),
            }
        }
        assert!(field.is_empty() && record.is_empty(), "ends with \\r\\n");
        records
    }

    #[test]
    fn days_become_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(20_819), (2027, 1, 1));
        assert_eq!(iso_utc(1_800_000_000), "2027-01-15T08:00:00Z");
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn every_panel_name_is_unique_and_checked() {
        for (i, p) in PANELS.iter().enumerate() {
            assert!(PANELS[..i].iter().all(|q| q.name != p.name), "{}", p.name);
            assert_eq!(panel(Some(p.name)).unwrap(), *p);
            // Names go into the file name: nothing to escape.
            assert!(
                p.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_')
            );
        }
        for bad in [None, Some(""), Some("trend\""), Some("../x"), Some("fetch")] {
            let e = panel(bad).unwrap_err();
            assert_eq!(e.body()["error"]["field"], "panel");
        }
    }

    #[test]
    fn rows_flatten_and_nulls_stay_empty() {
        let v = json!({
            "trend": [
                {"at": 3600, "harvested": 5, "searchable": null},
                {"at": 7200, "harvested": -3, "searchable": 0}
            ],
            "previous": {"trend": [{"at": 0, "harvested": 1, "searchable": 2}]},
            "funnel": [{"cohort": 3600, "final": true, "stages": {"stored": 4}}],
        });
        let p = panel(Some("trend")).unwrap();
        let text = render(p.columns, &rows(&p, &v, true));
        assert_eq!(
            text,
            "period,at,at_utc,harvested,searchable,searchable_imported\r\n\
             current,3600,1970-01-01T01:00:00Z,5,,\r\n\
             current,7200,1970-01-01T02:00:00Z,-3,0,\r\n\
             previous,0,1970-01-01T00:00:00Z,1,2,\r\n"
        );
        assert_eq!(render(p.columns, &rows(&p, &v, false)).lines().count(), 3);
        // A field a row lacks is an empty cell; one beyond the columns
        // is added.
        let f = render(
            &["cohort", "final"],
            &rows(&panel(Some("funnel")).unwrap(), &v, false),
        );
        assert_eq!(
            f,
            "period,cohort,cohort_utc,final,stages.stored\r\n\
             current,3600,1970-01-01T01:00:00Z,true,4\r\n"
        );
        // Nothing to list: the header alone, all of it.
        assert_eq!(render(&["at", "kind"], &[]), "period,at,at_utc,kind\r\n");
    }

    /// Every panel lists its columns sorted, but annotations, which keep
    /// their pre-IMP-5 `at,kind,text` first (B-refetch-funnel).
    #[test]
    fn columns_are_sorted_but_the_annotations_lead() {
        for p in &PANELS {
            let mut sorted = p.columns.to_vec();
            sorted.sort();
            if p.name == "annotations" {
                assert_eq!(&p.columns[..3], ["at", "kind", "text"]);
                assert_eq!(&p.columns[3..], &sorted[1..6]);
            } else {
                assert_eq!(p.columns, sorted, "{}", p.name);
            }
            // Each panel's export, rendered from the endpoint's fixture
            // (`tests/insights.rs`, blessed into the goldens), has exactly
            // the declared header: a field the panel doesn't declare would
            // land in it and move the columns.
            let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/golden")
                .join(format!("insights_export_{}.txt", p.name.replace('.', "_")));
            let text = std::fs::read_to_string(&golden)
                .unwrap_or_else(|e| panic!("{}: {e}", golden.display()));
            let mut header = vec!["period".to_string()];
            for c in p.columns {
                header.push(c.to_string());
                if TIMES.contains(c) {
                    header.push(format!("{c}_utc"));
                }
            }
            assert_eq!(
                text.lines().next(),
                Some(header.join(",").as_str()),
                "{}",
                p.name
            );
        }
        let v = json!({"annotations": [
            {"at": 3600, "kind": "import_removed", "text": "job=1 source=s removed=2",
             "import": {"job": 1, "merged": null, "new": null, "removed": 2, "source": "s"}},
            {"at": 7200, "kind": "paused", "text": "", "import": null}
        ]});
        let p = panel(Some("annotations")).unwrap();
        let recs = parse(&render(p.columns, &rows(&p, &v, false)));
        assert_eq!(
            recs[0],
            [
                "period",
                "at",
                "at_utc",
                "kind",
                "text",
                "import.job",
                "import.merged",
                "import.new",
                "import.removed",
                "import.source"
            ]
        );
        assert_eq!(
            recs[1][3..],
            [
                "import_removed",
                "job=1 source=s removed=2",
                "1",
                "",
                "",
                "2",
                "s"
            ]
        );
        assert_eq!(recs[2][3..], ["paused", "", "", "", "", "", ""]);
    }

    #[test]
    fn text_is_defused_and_numbers_are_not() {
        let v = json!({"segments": {"by_source": [
            {"key": "=HYPERLINK(\"http://x\",\"y\")", "harvested": -3},
            {"key": "@SUM(A1)", "harvested": 1},
            {"key": "\tcmd", "harvested": 2},
            {"key": "\r=x", "harvested": 2},
            {"key": "a\u{202E}b", "harvested": 2},
            {"key": "say \"hi\",\nthen", "harvested": 2},
            {"key": "+1", "harvested": 2},
            {"key": "-1", "harvested": 2},
        ]}});
        let p = panel(Some("segments.by_source")).unwrap();
        let text = render(p.columns, &rows(&p, &v, false));
        let recs = parse(&text);
        assert_eq!(
            recs[0],
            ["period", "harvested", "key", "searchable", "yield"]
        );
        let keys: Vec<&str> = recs[1..].iter().map(|r| r[2].as_str()).collect();
        assert_eq!(
            keys,
            [
                "'=HYPERLINK(\"http://x\",\"y\")",
                "'@SUM(A1)",
                "'\\u{9}cmd",
                "'\\u{d}=x",
                "a\\u{202e}b",
                "say \"hi\",\\u{a}then",
                "'+1",
                "'-1",
            ]
        );
        assert_eq!(recs[1][1], "-3", "a number stays a number");
    }

    proptest! {
        /// Any text survives a CSV reader unchanged but for the defusing
        /// prefix and `\u{…}` escapes, and never starts a formula.
        #[test]
        fn any_text_round_trips_defused(s in "\\PC*|[=+@\\-\\t\\r\\n\",\u{202e}a-z]{0,12}") {
            let v = json!({"segments": {"by_group": [{"key": s.clone(), "searchable": 1}]}});
            let p = panel(Some("segments.by_group")).unwrap();
            let text = render(p.columns, &rows(&p, &v, false));
            let recs = parse(&text);
            prop_assert_eq!(recs.len(), 2);
            prop_assert_eq!(recs[1].len(), 3);
            let got = &recs[1][1];
            prop_assert!(!got.starts_with(['=', '+', '-', '@', '\t', '\r']), "{:?}", got);
            // The reader gives back exactly the defused cell, unquoted.
            prop_assert_eq!(got, &unquote(&csv_cell(&s)));
            // Nothing but the defusing changed: no character is lost.
            prop_assert!(got.len() >= s.len());
        }
    }

    /// What a reader gives back for `cell`, a [`csv_cell`].
    fn unquote(cell: &str) -> String {
        match cell.strip_prefix('"').and_then(|c| c.strip_suffix('"')) {
            Some(inner) => inner.replace("\"\"", "\""),
            None => cell.to_string(),
        }
    }
}
