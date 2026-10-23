//! The digest (§10.9.7): `GET /digest/preview`, and [`render`] for the
//! sender that follows.
//!
//! - **Inputs, and nothing else.** One [`insights::overview_response`] call
//!   with `range=24h` (daily) or `range=7d` (weekly) and `compare=1`, so
//!   every number, Δ, conversion, share and percentile is the server's own
//!   and "Show adult content" is applied by the builder. Segment movers
//!   come from [`insights::segment_movers`]. This module only picks and
//!   formats: no sums, rates, Δs or store reads.
//! - **Null is `—`**, never 0 (`docs/metrics.md` §1); `≈` only where the
//!   field is always approximate (Harvested, time to searchable).
//! - **Text** is plain UTF-8 in at most [`MAX_TEXT_BYTES`]; **HTML** is
//!   built by [`Html`], which escapes by default and takes tags only as
//!   `&'static str`. Every string that comes from the response and isn't a
//!   constant (segment keys, exception kinds, reasons) goes through
//!   [`clean`] first.
//! - **Links are root-relative paths**: there is no configured public
//!   address yet, so the sender makes them absolute.
//! - English only. The formatters here reproduce `docs/metrics.md` §3's
//!   `en` column; the web client's `format/` is the other copy.

use std::fmt::Write as _;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use utoipa::OpenApi;

pub use crate::dto::digest::{DigestPeriod, DigestPreview, DigestQuery};
use crate::dto::insights::{
    Conversion, HealthState, InsightsException, InsightsRange, OverviewQuery, OverviewResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::extract::Query;
use crate::insights::{self, civil_from_days};
use crate::{ApiState, auth};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(preview))]
pub(crate) struct ApiDoc;

pub fn routes() -> Router<ApiState> {
    auth::protected(Router::new().route("/digest/preview", get(preview)))
}

/// The most the text may take (ntfy's message and Discord's description
/// are both 4,096 bytes).
const MAX_TEXT_BYTES: usize = 4_000;
/// The most the HTML may take (the email channel's 96 KiB part).
const MAX_HTML_BYTES: usize = 96 * 1024;
/// The most a subject may take.
const MAX_SUBJECT_CHARS: usize = 120;
/// Longest string from the response, in characters, `…` included.
const MAX_FIELD_CHARS: usize = 64;
/// Segment movers shown.
const MOVERS: usize = 3;
/// Loss reasons shown.
const LOSSES: usize = 3;

const DASH: &str = "—";
const MINUS: char = '\u{2212}';
const NBSP: char = '\u{a0}';
const UP: &str = "▲";
const DOWN: &str = "▼";
const NO_COHORTS: &str = "No completed cohorts yet";

/// A rendered digest.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Digest {
    /// Unix seconds the numbers were read.
    pub generated_at: i64,
    pub html: String,
    pub range: InsightsRange,
    pub subject: String,
    pub text: String,
}

impl DigestPeriod {
    /// The overview `range` this period reads, and the Insights link's.
    fn range(self) -> &'static str {
        match self {
            DigestPeriod::Daily => "24h",
            DigestPeriod::Weekly => "7d",
        }
    }

    /// What the Δs are against.
    fn versus(self) -> &'static str {
        match self {
            DigestPeriod::Daily => "vs previous 24 hours",
            DigestPeriod::Weekly => "vs previous 7 days",
        }
    }
}

/// The digest for `period`, read and rendered now. The sender follow-up
/// calls this (and adds absolute links, the schedule and the channels).
pub(crate) async fn digest(state: &ApiState, period: DigestPeriod) -> ApiResult<Digest> {
    let overview = insights::overview_response(
        state,
        OverviewQuery {
            range: Some(period.range().into()),
            compare: Some(1),
            ..OverviewQuery::default()
        },
    )
    .await?;
    Ok(render(&overview, period, overview.updated_at))
}

/// The digest as it would be sent now (§10.9.7). JSON only: the HTML is
/// shown by the client in `<iframe sandbox srcdoc>`, the text in a `<pre>`.
#[utoipa::path(
    operation_id = "getDigestPreview",
    get,
    path = "/digest/preview",
    tag = "insights",
    params(DigestQuery),
    responses((status = 200, body = DigestPreview))
)]
async fn preview(
    State(s): State<ApiState>,
    Query(q): Query<DigestQuery>,
) -> ApiResult<Json<DigestPreview>> {
    let period = match q.period.as_deref() {
        None | Some("weekly") => DigestPeriod::Weekly,
        Some("daily") => DigestPeriod::Daily,
        Some(_) => {
            return Err(
                ApiError::bad_request("bad_parameter", "period is daily or weekly.")
                    .field("period"),
            );
        }
    };
    let d = digest(&s, period).await?;
    Ok(Json(DigestPreview {
        generated_at: d.generated_at,
        html: d.html,
        period,
        range: d.range,
        subject: d.subject,
        text: d.text,
    }))
}

// ---------------------------------------------------------------------
// Formatters: `docs/metrics.md` §3's `en` column.
// ---------------------------------------------------------------------

/// `formatCount`: `0`, `1,284`, `84,210`.
fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `x` × 10^`k`, rounded half away from zero on `x`'s shortest decimal
/// form, as `Intl.NumberFormat` does (0.575 → 58 at k = 2, where the binary
/// double is just under it). Saturates; `x` is finite and not negative.
fn scaled_round(x: f64, k: usize) -> u64 {
    let s = format!("{x}");
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let mut digits = String::from(int);
    let mut frac = frac.chars();
    for _ in 0..k {
        digits.push(frac.next().unwrap_or('0'));
    }
    let n = digits.parse::<u64>().unwrap_or(u64::MAX);
    if frac.next().is_some_and(|c| c >= '5') {
        n.saturating_add(1)
    } else {
        n
    }
}

/// `n` steps of 10^-`f` percent, as `12.3%` (`f` = 1) or `12%` (`f` = 0).
fn percent_steps(n: u64, f: u32) -> String {
    if f == 0 {
        format!("{}%", format_count(n))
    } else {
        format!("{}.{}%", format_count(n / 10), n % 10)
    }
}

/// `formatPercent` of a 0–1 ratio with `f` fraction digits (0 or 1): a
/// non-zero value that would print as 0 is `<0.1%` (`<1%`), one below 1
/// that would print as 100% is `>99.9%` (`>99%`). Negative or unusable
/// input is `—`.
fn format_percent(ratio: f64, f: u32) -> String {
    if !ratio.is_finite() || ratio < 0.0 {
        return DASH.into();
    }
    let one = 10u64.pow(2 + f);
    let steps = scaled_round(ratio, 2 + f as usize);
    if ratio > 0.0 && steps == 0 {
        format!("<{}", percent_steps(1, f))
    } else if ratio < 1.0 && steps >= one {
        format!(">{}", percent_steps(one - 1, f))
    } else {
        percent_steps(steps, f)
    }
}

/// `formatDelta` of a count: `+1,240`, `−3`, `0`.
fn format_delta_count(d: i64) -> String {
    match d {
        0 => "0".into(),
        d if d > 0 => format!("+{}", format_count(d.unsigned_abs())),
        d => format!("{MINUS}{}", format_count(d.unsigned_abs())),
    }
}

/// `formatDelta` of a ratio in percent points (f = 1): `−3.2%`, `0.0%`,
/// `+3.2%`. A change that rounds to zero prints unsigned.
fn format_delta_percent(d: f64) -> String {
    if !d.is_finite() {
        return DASH.into();
    }
    let steps = scaled_round(d.abs(), 3);
    match steps {
        0 => "0.0%".into(),
        s if d > 0.0 => format!("+{}", percent_steps(s, 1)),
        s => format!("{MINUS}{}", percent_steps(s, 1)),
    }
}

/// `formatSpan` of seconds: `45 sec`, `12 min`, `5.2 hr`, `3.1 days`.
fn format_span(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return DASH.into();
    }
    // (unit, seconds per unit, the seconds at which the next unit takes
    // over)
    const STEPS: [(&str, f64, f64); 4] = [
        ("sec", 1.0, 60.0),
        ("min", 60.0, 3_600.0),
        ("hr", 3_600.0, 172_800.0),
        ("day", 86_400.0, f64::INFINITY),
    ];
    for (unit, per, upto) in STEPS {
        if seconds >= upto {
            continue;
        }
        let v = seconds / per;
        let rounded = round_to(v, if v < 10.0 { 1 } else { 0 });
        // Rounding must not reach the next unit ("60 sec").
        if rounded * per < upto {
            let tenths = (rounded * 10.0).round() as u64;
            let number = match tenths % 10 {
                0 => format_count(tenths / 10),
                frac => format!("{}.{frac}", format_count(tenths / 10)),
            };
            let plural = unit == "day" && number != "1";
            return format!("{number} {unit}{}", if plural { "s" } else { "" });
        }
    }
    DASH.into()
}

fn round_to(v: f64, digits: i32) -> f64 {
    let k = 10f64.powi(digits);
    (v * k).round() / k
}

/// `formatDelta` of seconds, in `formatSpan`'s unit: `+5.2 hr`, `−45 sec`.
fn format_delta_span(d: i64) -> String {
    let span = format_span(d.unsigned_abs() as f64);
    match d {
        0 => span,
        d if d > 0 => format!("+{span}"),
        _ => format!("{MINUS}{span}"),
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `formatDate` in UTC: `Oct 22, 2026`.
fn format_date(seconds: i64) -> String {
    let (y, m, d) = civil_from_days(seconds.div_euclid(86_400));
    format!("{} {d}, {y}", MONTHS[(m - 1) as usize])
}

/// `formatDateTime` in UTC: `Oct 22, 2026, 14:05`.
fn format_date_time(seconds: i64) -> String {
    let minutes = seconds.rem_euclid(86_400) / 60;
    format!(
        "{}, {:02}:{:02}",
        format_date(seconds),
        minutes / 60,
        minutes % 60
    )
}

/// `≈` + U+00A0 + the value: for the always-approximate numbers.
fn approx(value: &str) -> String {
    format!("≈{NBSP}{value}")
}

// ---------------------------------------------------------------------
// Untrusted strings, links, HTML.
// ---------------------------------------------------------------------

/// A string from the response, made safe to print: controls (C0, DEL, C1),
/// bidi formatting characters and line separators removed, cut to
/// [`MAX_FIELD_CHARS`] characters with `…`.
fn clean(s: &str) -> String {
    let kept: Vec<char> = s
        .chars()
        .filter(|&c| {
            !c.is_control()
                && !matches!(
                    c,
                    '\u{61c}'
                        | '\u{200e}'
                        | '\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{2028}'
                        | '\u{2029}'
                )
        })
        .collect();
    let mut out: String = if kept.len() > MAX_FIELD_CHARS {
        kept[..MAX_FIELD_CHARS - 1].iter().collect()
    } else {
        kept.iter().collect()
    };
    if kept.len() > MAX_FIELD_CHARS {
        out.push('…');
    }
    if out.is_empty() {
        "(blank)".into()
    } else {
        out
    }
}

/// Whether `target` may be part of a link path: `^[a-z_]{1,32}$`.
fn valid_target(target: &str) -> bool {
    (1..=32).contains(&target.len()) && target.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// An exception's fix as a root-relative path, or none (an unknown kind,
/// or a target that isn't a plain name).
fn fix_path(e: &InsightsException) -> Option<String> {
    if !valid_target(&e.fix.target) {
        return None;
    }
    match e.fix.kind.as_str() {
        "stage" => Some(format!("/insights/{}", e.fix.target)),
        "settings" => Some(format!("/settings/{}", e.fix.target)),
        _ => None,
    }
}

/// The HTML builder. Escapes by default: data goes in with [`text`] and
/// is escaped; tags go in with [`raw`], which takes only `&'static str`,
/// so nothing built at run time can reach the output unescaped.
///
/// [`text`]: Html::text
/// [`raw`]: Html::raw
#[derive(Debug, Default)]
struct Html(String);

impl Html {
    fn raw(&mut self, markup: &'static str) -> &mut Self {
        self.0.push_str(markup);
        self
    }

    fn text(&mut self, s: &str) -> &mut Self {
        escape_into(s, &mut self.0);
        self
    }

    /// A direction arrow, hidden from assistive technology (the sign
    /// beside it says the same).
    fn arrow(&mut self, arrow: &'static str) -> &mut Self {
        if !arrow.is_empty() {
            self.raw("<span aria-hidden=\"true\">")
                .raw(arrow)
                .raw("</span> ");
        }
        self
    }
}

/// `&`, `<`, `>`, `"` and `'` as entities.
fn escape_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

// ---------------------------------------------------------------------
// The model both renderers read.
// ---------------------------------------------------------------------

/// A Δ: the arrow (empty when there's no direction) and the signed text.
struct Change {
    /// `p50 ` etc.
    prefix: &'static str,
    arrow: &'static str,
    text: String,
}

impl Change {
    fn new(prefix: &'static str, text: String) -> Change {
        let arrow = if text.starts_with('+') {
            UP
        } else if text.starts_with(MINUS) {
            DOWN
        } else {
            ""
        };
        Change {
            prefix,
            arrow,
            text,
        }
    }

    fn plain(&self) -> String {
        let arrow = if self.arrow.is_empty() { "" } else { " " };
        format!("{}{}{arrow}{}", self.prefix, self.arrow, self.text)
    }
}

struct Row {
    label: &'static str,
    value: String,
    changes: Vec<Change>,
}

struct Stage {
    label: &'static str,
    count: String,
    /// The conversion text and what it's of; none for the first stage.
    conversion: Option<(String, &'static str)>,
}

struct Mover {
    key: String,
    list: &'static str,
    change: Change,
}

struct Alert {
    severity: &'static str,
    message: String,
    link: Option<String>,
}

struct Model {
    subject: String,
    period: String,
    generated: String,
    versus: Option<&'static str>,
    rows: Vec<Row>,
    cohorts: String,
    stages: Vec<Stage>,
    losses: Vec<String>,
    movers: Vec<Mover>,
    alerts: Vec<Alert>,
    insights: String,
}

fn optional_count(n: Option<u64>) -> String {
    n.map_or_else(|| DASH.into(), format_count)
}

fn conversion(c: Option<f64>) -> String {
    c.map_or_else(|| DASH.into(), |r| format_percent(r, 1))
}

fn health_reason(reason: &str) -> String {
    match reason {
        "paused" => "paused".into(),
        "writer_stalled" => "the writer has stalled".into(),
        "index_lag" => "the search index is behind".into(),
        other => clean(other),
    }
}

/// A value in the unit the exception says, or none for a unit this
/// module doesn't know.
fn exception_value(e: &InsightsException) -> Option<String> {
    let v = e.value?;
    match e.unit.as_deref()? {
        "s" => Some(format_span(v)),
        "torrents" if v >= 0.0 => Some(format_count(v.round() as u64)),
        "ratio" => Some(format_percent(v, 1)),
        _ => None,
    }
}

fn alert(e: &InsightsException) -> Alert {
    let value = exception_value(e);
    let message = match (e.kind.as_str(), value) {
        ("index_lag", Some(v)) => format!("The search index is {v} behind."),
        ("index_lag", None) => "The search index is behind.".into(),
        ("cohort_stalled", Some(v)) => {
            format!("Nothing was stored for 2 hours while {v} torrents are in progress.")
        }
        ("cohort_stalled", None) => {
            "Nothing was stored for 2 hours while torrents are in progress.".into()
        }
        ("yield_below_baseline", Some(v)) => {
            format!("Yield is {v} for the newest completed cohorts, below the 7 days before them.")
        }
        ("yield_below_baseline", None) => {
            "Yield for the newest completed cohorts is below the 7 days before them.".into()
        }
        (kind, value) => {
            let kind = clean(kind);
            match value {
                Some(v) => format!("Needs attention: {kind} ({v})."),
                None => format!("Needs attention: {kind}."),
            }
        }
    };
    Alert {
        severity: match e.severity {
            crate::dto::insights::Severity::Warning => "Warning",
            crate::dto::insights::Severity::Error => "Error",
        },
        message,
        link: fix_path(e),
    }
}

fn loss_name(reason: &str) -> String {
    match reason {
        "no_peers" => "No peers".into(),
        "timed_out" => "Timed out".into(),
        "refused" => "Refused".into(),
        "encryption_failed" => "Encryption failed".into(),
        "oversize" => "Too large".into(),
        "invalid" => "Invalid".into(),
        "blocked" => "Blocked".into(),
        "evicted" => "Evicted".into(),
        "gave_up" => "Gave up".into(),
        "unreachable" => "Unreachable".into(),
        other => clean(other),
    }
}

fn model(o: &OverviewResponse, period: DigestPeriod, now: i64) -> Model {
    let k = &o.kpi;
    let subject = match period {
        DigestPeriod::Daily => format!("Lodestar daily digest: {}", format_date(now)),
        DigestPeriod::Weekly => format!(
            "Lodestar weekly digest: {} – {}",
            format_date(o.range.from),
            format_date(o.range.to - 1)
        ),
    };

    let change = |prefix, d: Option<String>| d.map(|t| Change::new(prefix, t));
    let time = if k.time_to_searchable_p50_s.is_none() && k.time_to_searchable_p95_s.is_none() {
        format!("{DASH} ({NO_COHORTS})")
    } else {
        let part = |s: Option<u64>, name: &str| {
            s.map_or_else(|| DASH.to_string(), |s| approx(&format_span(s as f64)))
                + " ("
                + name
                + ")"
        };
        format!(
            "{}, {}",
            part(k.time_to_searchable_p50_s, "p50"),
            part(k.time_to_searchable_p95_s, "p95")
        )
    };
    let health = {
        let state = match k.health.state {
            HealthState::Running => "Running",
            HealthState::Paused => "Paused",
            HealthState::Degraded => "Degraded",
        };
        match &k.health.reason {
            Some(r) if r != "paused" || k.health.state != HealthState::Paused => {
                format!("{state} ({})", health_reason(r))
            }
            Some(_) => state.into(),
            None => state.into(),
        }
    };
    let rows = vec![
        Row {
            label: "Searchable torrents",
            value: optional_count(k.searchable),
            changes: change("", k.searchable_delta.map(format_delta_count))
                .into_iter()
                .collect(),
        },
        Row {
            label: "New searchable",
            value: format_count(k.new_searchable),
            changes: change("", k.new_searchable_delta.map(format_delta_count))
                .into_iter()
                .collect(),
        },
        Row {
            label: "Yield",
            value: k.yield_.map_or_else(
                || format!("{DASH} ({NO_COHORTS})"),
                |y| format_percent(y, 1),
            ),
            changes: change("", k.yield_delta.map(format_delta_percent))
                .into_iter()
                .collect(),
        },
        Row {
            label: "Time to searchable",
            value: time,
            changes: [
                change(
                    "p50 ",
                    k.time_to_searchable_p50_delta_s.map(format_delta_span),
                ),
                change(
                    "p95 ",
                    k.time_to_searchable_p95_delta_s.map(format_delta_span),
                ),
            ]
            .into_iter()
            .flatten()
            .collect(),
        },
        Row {
            label: "Health",
            value: health,
            changes: Vec::new(),
        },
    ];
    let versus = rows
        .iter()
        .any(|r| !r.changes.is_empty())
        .then(|| period.versus());

    let f = &o.funnel_total;
    let Conversion {
        fetched,
        peer_found,
        searchable,
        stored,
    } = f.conversion;
    let stage = |label, count: u64, c: Option<f64>, of| Stage {
        label,
        count: format_count(count),
        conversion: Some((conversion(c), of)),
    };
    let stages = if f.cohorts == 0 {
        Vec::new()
    } else {
        vec![
            Stage {
                label: "Harvested",
                count: approx(&format_count(f.stages.harvested)),
                conversion: None,
            },
            stage("Peer found", f.stages.peer_found, peer_found, "harvested"),
            stage("Metadata fetched", f.stages.fetched, fetched, "peer found"),
            stage("Stored", f.stages.stored, stored, "metadata fetched"),
            stage("Searchable", f.stages.searchable, searchable, "stored"),
        ]
    };
    let cohorts = match f.cohorts {
        0 => "No cohorts in this period yet".to_string(),
        n => format!(
            "{} {}, {} final",
            format_count(n),
            if n == 1 { "cohort" } else { "cohorts" },
            format_count(f.final_cohorts)
        ),
    };
    let mut shares: Vec<(&str, f64)> = f
        .loss_share
        .iter()
        .filter_map(|(r, s)| s.filter(|s| *s > 0.0).map(|s| (r.as_str(), s)))
        .collect();
    shares.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
    let losses = shares
        .into_iter()
        .take(LOSSES)
        .map(|(r, s)| format!("{} {}", loss_name(r), format_percent(s, 0)))
        .collect();

    let movers = o
        .previous
        .as_ref()
        .map(|p| insights::segment_movers(&o.segments, &p.segments, MOVERS))
        .unwrap_or_default()
        .into_iter()
        .map(|m| Mover {
            key: clean(&m.key),
            list: match m.list {
                "by_group" => "by group",
                _ => "by content type",
            },
            change: Change::new("", format_delta_count(m.delta)),
        })
        .collect();

    Model {
        subject,
        period: format!(
            "{} – {} UTC",
            format_date_time(o.range.from),
            format_date_time(o.range.to)
        ),
        generated: format!("{} UTC", format_date_time(now)),
        versus,
        rows,
        cohorts,
        stages,
        losses,
        movers,
        alerts: o.exceptions.iter().map(alert).collect(),
        insights: format!("/insights?range={}&compare=1", period.range()),
    }
}

// ---------------------------------------------------------------------
// Renderers.
// ---------------------------------------------------------------------

/// The plain-text part with at most `alerts` exceptions listed (and a
/// line saying how many more there are).
fn render_text(m: &Model, alerts: usize) -> String {
    let mut t = String::new();
    let _ = writeln!(t, "{}\n", m.subject);
    let _ = writeln!(t, "Period: {}; generated {}\n", m.period, m.generated);

    match m.versus {
        Some(v) => writeln!(t, "Key numbers (changes {v})"),
        None => writeln!(t, "Key numbers"),
    }
    .ok();
    for r in &m.rows {
        let _ = write!(t, "- {}: {}", r.label, r.value);
        if !r.changes.is_empty() {
            let changes: Vec<String> = r.changes.iter().map(Change::plain).collect();
            let _ = write!(t, "; change {}", changes.join(", "));
        }
        t.push('\n');
    }

    let _ = writeln!(t, "\nFunnel ({})", m.cohorts);
    for s in &m.stages {
        let _ = write!(t, "- {}: {}", s.label, s.count);
        if let Some((c, of)) = &s.conversion {
            let _ = write!(t, " ({c} of {of})");
        }
        t.push('\n');
    }
    if !m.stages.is_empty() {
        if m.losses.is_empty() {
            t.push_str("No losses.\n");
        } else {
            let _ = writeln!(t, "Biggest losses: {}", m.losses.join(", "));
        }
    }

    if !m.movers.is_empty() {
        let _ = writeln!(
            t,
            "\nTop movers ({})",
            m.versus.unwrap_or("vs previous period")
        );
        for mv in &m.movers {
            let _ = writeln!(t, "- {}, {}: {}", mv.key, mv.list, mv.change.plain());
        }
    }

    t.push_str("\nNeeds attention\n");
    if m.alerts.is_empty() {
        t.push_str("Nothing needs attention.\n");
    }
    for a in m.alerts.iter().take(alerts) {
        let _ = write!(t, "- {}: {}", a.severity, a.message);
        if let Some(link) = &a.link {
            let _ = write!(t, " Fix: {link}");
        }
        t.push('\n');
    }
    if m.alerts.len() > alerts {
        let _ = writeln!(t, "- And {} more in Insights.", m.alerts.len() - alerts);
    }

    let _ = writeln!(t, "\nOpen Insights: {}", m.insights);
    t
}

/// The HTML part with at most `alerts` exceptions listed.
fn render_html(m: &Model, alerts: usize) -> String {
    let mut h = Html::default();
    h.raw("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>")
        .text(&m.subject)
        .raw("</title>\n</head>\n<body>\n<h1>")
        .text(&m.subject)
        .raw("</h1>\n<p>Period: ")
        .text(&m.period)
        .raw("; generated ")
        .text(&m.generated)
        .raw("</p>\n<h2>Key numbers</h2>\n<table>\n<thead>\n<tr><th scope=\"col\">Metric</th><th scope=\"col\">Value</th>");
    match m.versus {
        Some(v) => {
            h.raw("<th scope=\"col\">Change ").text(v).raw("</th>");
        }
        None => {
            h.raw("<th scope=\"col\">Change</th>");
        }
    }
    h.raw("</tr>\n</thead>\n<tbody>\n");
    for r in &m.rows {
        h.raw("<tr><th scope=\"row\">")
            .text(r.label)
            .raw("</th><td style=\"text-align:right\">")
            .text(&r.value)
            .raw("</td><td>");
        for (i, c) in r.changes.iter().enumerate() {
            if i > 0 {
                h.raw(", ");
            }
            h.text(c.prefix).arrow(c.arrow).text(&c.text);
        }
        h.raw("</td></tr>\n");
    }
    h.raw("</tbody>\n</table>\n<h2>Funnel</h2>\n<p>")
        .text(&m.cohorts)
        .raw("</p>\n");
    if !m.stages.is_empty() {
        h.raw("<ul>\n");
        for s in &m.stages {
            h.raw("<li>").text(s.label).raw(": ").text(&s.count);
            if let Some((c, of)) = &s.conversion {
                h.raw(" (").text(c).raw(" of ").text(of).raw(")");
            }
            h.raw("</li>\n");
        }
        h.raw("</ul>\n<p>");
        if m.losses.is_empty() {
            h.raw("No losses.");
        } else {
            h.raw("Biggest losses: ").text(&m.losses.join(", "));
        }
        h.raw("</p>\n");
    }
    if !m.movers.is_empty() {
        h.raw("<h2>Top movers</h2>\n<ul>\n");
        for mv in &m.movers {
            h.raw("<li>")
                .text(&mv.key)
                .raw(", ")
                .text(mv.list)
                .raw(": ")
                .arrow(mv.change.arrow)
                .text(&mv.change.text)
                .raw("</li>\n");
        }
        h.raw("</ul>\n");
    }
    h.raw("<h2>Needs attention</h2>\n");
    if m.alerts.is_empty() {
        h.raw("<p>Nothing needs attention.</p>\n");
    } else {
        h.raw("<ul>\n");
        for a in m.alerts.iter().take(alerts) {
            h.raw("<li>").text(a.severity).raw(": ").text(&a.message);
            if let Some(link) = &a.link {
                h.raw(" Fix: <a href=\"")
                    .text(link)
                    .raw("\">")
                    .text(link)
                    .raw("</a>");
            }
            h.raw("</li>\n");
        }
        if m.alerts.len() > alerts {
            h.raw("<li>And ")
                .text(&format_count((m.alerts.len() - alerts) as u64))
                .raw(" more in Insights.</li>\n");
        }
        h.raw("</ul>\n");
    }
    h.raw("<p>Open Insights: <a href=\"")
        .text(&m.insights)
        .raw("\">")
        .text(&m.insights)
        .raw("</a></p>\n</body>\n</html>\n");
    h.0
}

/// Renders the digest for `period` from the overview it read (`range` 24h
/// or 7d, `compare=1`); `now` is the generation time. Pure.
pub(crate) fn render(o: &OverviewResponse, period: DigestPeriod, now: i64) -> Digest {
    let m = model(o, period, now);
    // Fewer exceptions until each part fits (they are the only part that
    // grows; every field is cut to 64 characters).
    let mut shown = m.alerts.len();
    let mut text = render_text(&m, shown);
    while text.len() > MAX_TEXT_BYTES && shown > 0 {
        shown -= 1;
        text = render_text(&m, shown);
    }
    let subject = m.subject.chars().take(MAX_SUBJECT_CHARS).collect();
    let mut shown_html = m.alerts.len();
    let mut html = render_html(&m, shown_html);
    while html.len() > MAX_HTML_BYTES && shown_html > 0 {
        shown_html -= 1;
        html = render_html(&m, shown_html);
    }
    Digest {
        generated_at: now,
        html,
        range: o.range.clone(),
        subject,
        text,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use proptest::prelude::*;

    use super::*;
    use crate::dto::insights::{
        Bucket, Fix, FunnelStages, FunnelTotal, Health, Kpi, PreviousKpi, PreviousPeriod,
        PreviousStages, SegmentCount, Segments, Serve, Severity, Stages,
    };

    /// 2026-10-22 14:05 UTC, a Thursday: `docs/metrics.md` §3's example.
    const T: i64 = 1_792_677_900;
    const HOUR: i64 = 3_600;

    // -- The formatters, from `docs/metrics.md` §3's `en` column ------------

    #[test]
    fn count_examples() {
        for (n, want) in [(0, "0"), (1_284, "1,284"), (84_210, "84,210")] {
            assert_eq!(format_count(n), want);
        }
        assert_eq!(format_count(1_000_000_000_000), "1,000,000,000,000");
        assert_eq!(format_count(999), "999");
    }

    #[test]
    fn percent_examples() {
        for (ratio, f, want) in [
            (0.374, 1, "37.4%"),
            (0.82, 0, "82%"),
            (0.0, 1, "0.0%"),
            (0.0, 0, "0%"),
            (0.0004, 1, "<0.1%"),
            (0.0005, 1, "0.1%"),
            (0.001, 1, "0.1%"),
            (0.9996, 1, ">99.9%"),
            (0.9994, 1, "99.9%"),
            (0.004, 0, "<1%"),
            (0.005, 0, "1%"),
            (0.996, 0, ">99%"),
            (1.0, 1, "100.0%"),
            (1.0, 0, "100%"),
            (1.5, 1, "150.0%"),
            (-0.1, 1, "—"),
            // Half-way on the shortest decimal form, as Node rounds them.
            (0.575, 0, "58%"),
            (0.145, 0, "15%"),
            (0.285, 0, "29%"),
            (0.5005, 1, "50.1%"),
        ] {
            assert_eq!(format_percent(ratio, f), want, "{ratio} f={f}");
        }
    }

    #[test]
    fn delta_examples() {
        assert_eq!(format_delta_count(1_240), "+1,240");
        assert_eq!(format_delta_count(-3), "\u{2212}3");
        assert_eq!(format_delta_count(0), "0");
        assert_eq!(format_delta_count(1_000_000_000_000), "+1,000,000,000,000");
        assert_eq!(format_delta_percent(-0.032), "\u{2212}3.2%");
        assert_eq!(format_delta_percent(0.032), "+3.2%");
        assert_eq!(format_delta_percent(-0.5105), "\u{2212}51.1%");
        assert_eq!(format_delta_percent(0.0), "0.0%");
        assert_eq!(format_delta_percent(0.00001), "0.0%");
        assert_eq!(format_delta_percent(-0.00001), "0.0%");
        assert!(!format_delta_count(-12_345).contains('-'));
        assert!(!format_delta_percent(-0.5).contains('-'));
    }

    #[test]
    fn span_examples() {
        for (s, want) in [
            (45.0, "45 sec"),
            (12.0 * 60.0, "12 min"),
            (5.2 * 3600.0, "5.2 hr"),
            (3.1 * 86_400.0, "3.1 days"),
            (0.0, "0 sec"),
            (59.0, "59 sec"),
            (59.6, "1 min"),
            (60.0, "1 min"),
            (3_599.0, "1 hr"),
            (3_600.0, "1 hr"),
            (172_799.0, "2 days"),
            (172_800.0, "2 days"),
            (47.0 * 3600.0, "47 hr"),
            (9.96 * 3600.0, "10 hr"),
            (1e9, "11,574 days"),
            (-1.0, "—"),
        ] {
            assert_eq!(format_span(s), want, "{s} s");
        }
    }

    #[test]
    fn span_deltas_are_signed_in_one_unit() {
        assert_eq!(format_delta_span(0), "0 sec");
        assert_eq!(format_delta_span(-45), "\u{2212}45 sec");
        assert_eq!(format_delta_span(18_720), "+5.2 hr");
    }

    #[test]
    fn date_examples() {
        assert_eq!(format_date(T), "Oct 22, 2026");
        assert_eq!(format_date(0), "Jan 1, 1970");
        assert_eq!(format_date(-86_400), "Dec 31, 1969");
        assert_eq!(format_date_time(T), "Oct 22, 2026, 14:05");
        assert_eq!(
            format_date_time(T - 14 * HOUR - 5 * 60 + 5 * 60),
            "Oct 22, 2026, 00:05"
        );
    }

    // -- Escaping ------------------------------------------------------------

    fn unescape(s: &str) -> String {
        s.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
    }

    /// Every `&` starts one of the five entities we write.
    fn only_known_entities(s: &str) -> bool {
        s.match_indices('&').all(|(i, _)| {
            ["&amp;", "&lt;", "&gt;", "&quot;", "&#39;"]
                .iter()
                .any(|e| s[i..].starts_with(e))
        })
    }

    proptest! {
        #[test]
        fn text_never_leaves_markup_and_unescapes_back(s in ".{0,200}") {
            let mut h = Html::default();
            h.text(&s);
            prop_assert!(!h.0.contains(['<', '>', '"', '\'']));
            prop_assert!(only_known_entities(&h.0));
            prop_assert_eq!(unescape(&h.0), s);
        }

        #[test]
        fn text_of_markup_heavy_input(s in "[<>&\"'a-z;#0-9 ]{0,100}") {
            let mut h = Html::default();
            h.text(&s);
            prop_assert!(!h.0.contains(['<', '>', '"', '\'']));
            prop_assert!(only_known_entities(&h.0));
            prop_assert_eq!(unescape(&h.0), s);
        }

        #[test]
        fn clean_is_short_and_printable(s in ".{0,300}") {
            let c = clean(&s);
            prop_assert!(c.chars().count() <= MAX_FIELD_CHARS);
            prop_assert!(!c.chars().any(|c| c.is_control()));
            let bidi = ['\u{202e}', '\u{2066}', '\u{200f}', '\u{061c}'];
            prop_assert!(!c.contains(bidi));
        }
    }

    #[test]
    fn clean_removes_controls_and_bidi_and_cuts() {
        assert_eq!(
            clean("a\u{0}b\u{7f}c\u{85}d\ne\u{202e}f\u{2069}g"),
            "abcdefg"
        );
        assert_eq!(clean("\u{202e}\u{200f}"), "(blank)");
        let long = "x".repeat(200);
        let c = clean(&long);
        assert_eq!(c.chars().count(), 64);
        assert!(c.ends_with('…'));
        assert_eq!(clean(&"y".repeat(64)), "y".repeat(64));
    }

    #[test]
    fn targets_are_plain_names() {
        for ok in ["index", "fetch", "insights_digest", &"a".repeat(32)] {
            assert!(valid_target(ok), "{ok}");
        }
        for bad in [
            "",
            "Index",
            "a-b",
            "a/b",
            "../x",
            "a b",
            "a1",
            &"a".repeat(33),
            "é",
        ] {
            assert!(!valid_target(bad), "{bad}");
        }
    }

    // -- Fixtures ------------------------------------------------------------

    fn range(days: i64) -> InsightsRange {
        InsightsRange {
            bucket: Bucket::Hour,
            from: (T / HOUR + 1 - days * 24) * HOUR,
            to: (T / HOUR + 1) * HOUR,
            tz: "UTC".into(),
        }
    }

    fn stages(h: u64, p: u64, f: u64, st: u64, s: u64) -> FunnelStages {
        FunnelStages {
            fetched: f,
            harvested: h,
            peer_found: p,
            searchable: s,
            stored: st,
        }
    }

    fn segments() -> Segments {
        Segments {
            by_content_type: Vec::new(),
            by_group: Vec::new(),
            by_source: Vec::new(),
        }
    }

    fn row(key: &str, n: u64) -> SegmentCount {
        SegmentCount {
            key: key.into(),
            searchable: n,
        }
    }

    fn fix(kind: &str, target: &str) -> Fix {
        Fix {
            kind: kind.into(),
            target: target.into(),
        }
    }

    fn exception(kind: &str, unit: Option<&str>, value: Option<f64>, f: Fix) -> InsightsException {
        InsightsException {
            fix: f,
            kind: kind.into(),
            severity: Severity::Warning,
            since: T - 600,
            unit: unit.map(Into::into),
            value,
        }
    }

    /// A normal week, with Compare.
    fn week() -> OverviewResponse {
        let by_group = vec![row("video", 900), row("audio", 200), row("other", 40)];
        let prev_group = vec![row("video", 700), row("audio", 260), row("software", 30)];
        let mut loss_share = BTreeMap::new();
        for (r, s) in [
            ("no_peers", Some(0.5)),
            ("timed_out", Some(0.3)),
            ("refused", Some(0.1)),
            ("invalid", Some(0.06)),
            ("blocked", Some(0.04)),
            ("oversize", Some(0.0)),
            ("gave_up", None),
        ] {
            loss_share.insert(r.to_string(), s);
        }
        OverviewResponse {
            annotations: Vec::new(),
            exceptions: Vec::new(),
            funnel: Vec::new(),
            funnel_total: FunnelTotal {
                cohorts: 168,
                conversion: Conversion {
                    fetched: Some(0.625),
                    peer_found: Some(0.78),
                    searchable: Some(0.9996),
                    stored: Some(0.98),
                },
                final_cohorts: 96,
                in_progress: 40,
                loss_share,
                losses: BTreeMap::new(),
                stages: stages(10_234, 7_982, 4_989, 4_889, 4_887),
            },
            ingest_per_hour: Some(12),
            kpi: Kpi {
                health: Health {
                    reason: None,
                    state: HealthState::Running,
                },
                imported: Some(40),
                new_searchable: 1_240,
                new_searchable_delta: Some(-3),
                searchable: Some(84_210),
                searchable_delta: Some(1_240),
                time_to_searchable_p50_s: Some(720),
                time_to_searchable_p50_delta_s: Some(-45),
                time_to_searchable_p95_s: Some(5 * 3_600 + 720),
                time_to_searchable_p95_delta_s: Some(18_720),
                yield_: Some(0.374),
                yield_delta: Some(-0.032),
            },
            previous: Some(PreviousPeriod {
                kpi: PreviousKpi {
                    new_searchable: 1_243,
                    searchable: Some(82_970),
                    time_to_searchable_p50_s: Some(765),
                    time_to_searchable_p95_s: Some(0),
                    yield_: Some(0.406),
                },
                range: InsightsRange {
                    from: range(7).from - 7 * 86_400,
                    to: range(7).from,
                    ..range(7)
                },
                segments: Segments {
                    by_group: prev_group,
                    ..segments()
                },
                serve: Serve::default(),
                stages: PreviousStages {
                    discover: 0,
                    fetch: None,
                    serve: None,
                    understand: 0,
                },
                trend: Vec::new(),
            }),
            range: range(7),
            segments: Segments {
                by_group,
                ..segments()
            },
            serve: Serve::default(),
            stages: Stages {
                discover: 0,
                fetch: None,
                index: None,
                serve: None,
                understand: 0,
            },
            stale: None,
            trend: Vec::new(),
            updated_at: T,
        }
    }

    /// A day: no final cohort yet, Compare data but nothing to compare.
    fn day_no_final() -> OverviewResponse {
        let mut o = week();
        o.range = range(1);
        o.kpi.yield_ = None;
        o.kpi.yield_delta = None;
        o.kpi.time_to_searchable_p50_s = None;
        o.kpi.time_to_searchable_p50_delta_s = None;
        o.kpi.time_to_searchable_p95_s = None;
        o.kpi.time_to_searchable_p95_delta_s = None;
        o.kpi.searchable_delta = None;
        o.kpi.new_searchable = 0;
        o.kpi.new_searchable_delta = Some(0);
        o.funnel_total.final_cohorts = 0;
        o.funnel_total.cohorts = 1;
        o.funnel_total.stages = stages(9, 3, 2, 2, 0);
        o.funnel_total.conversion = Conversion {
            fetched: Some(0.6666),
            peer_found: Some(0.3333),
            searchable: Some(0.0),
            stored: Some(1.0),
        };
        o.funnel_total.loss_share = BTreeMap::new();
        o.previous = None;
        o
    }

    fn no_compare() -> OverviewResponse {
        let mut o = week();
        o.previous = None;
        let k = &mut o.kpi;
        k.new_searchable_delta = None;
        k.searchable_delta = None;
        k.time_to_searchable_p50_delta_s = None;
        k.time_to_searchable_p95_delta_s = None;
        k.yield_delta = None;
        o
    }

    fn paused() -> OverviewResponse {
        let mut o = week();
        o.kpi.health = Health {
            reason: Some("paused".into()),
            state: HealthState::Paused,
        };
        o.kpi.searchable = None;
        o.kpi.searchable_delta = None;
        o
    }

    fn no_movers() -> OverviewResponse {
        let mut o = week();
        o.previous.as_mut().unwrap().segments = o.segments.clone();
        o
    }

    fn exceptions() -> OverviewResponse {
        let mut o = week();
        o.kpi.health = Health {
            reason: Some("index_lag".into()),
            state: HealthState::Degraded,
        };
        o.exceptions = vec![
            exception("index_lag", Some("s"), Some(42.0), fix("stage", "index")),
            exception(
                "cohort_stalled",
                Some("torrents"),
                Some(1_200.0),
                fix("settings", "crawler"),
            ),
            exception(
                "yield_below_baseline",
                Some("ratio"),
                Some(0.21),
                fix("stage", "Fetch/../x"),
            ),
            exception("disk_projection", Some("s"), None, fix("stage", "storage")),
            exception("mystery", Some("furlongs"), Some(3.0), fix("page", "x")),
        ];
        o.exceptions[1].severity = Severity::Error;
        o
    }

    fn golden(name: &str, d: &Digest) {
        let got = format!(
            "subject: {}\ngenerated_at: {}\n\n=== text ({} bytes) ===\n{}\n=== html ({} bytes) ===\n{}",
            d.subject,
            d.generated_at,
            d.text.len(),
            d.text,
            d.html.len(),
            d.html
        );
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(format!("digest_render_{name}.txt"));
        if std::env::var_os("LODESTAR_BLESS").is_some() {
            std::fs::write(&path, &got).unwrap();
        } else {
            let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                panic!("{}: {e} (bless with LODESTAR_BLESS=1)", path.display())
            });
            assert_eq!(
                got,
                want.replace("\r\n", "\n"),
                "{name}: the digest changed"
            );
        }
    }

    #[test]
    fn goldens() {
        golden("week", &render(&week(), DigestPeriod::Weekly, T));
        golden(
            "day_no_final_cohort",
            &render(&day_no_final(), DigestPeriod::Daily, T),
        );
        golden("paused", &render(&paused(), DigestPeriod::Weekly, T));
        golden("no_movers", &render(&no_movers(), DigestPeriod::Weekly, T));
        golden(
            "exceptions",
            &render(&exceptions(), DigestPeriod::Weekly, T),
        );
        golden(
            "no_compare",
            &render(&no_compare(), DigestPeriod::Weekly, T),
        );
    }

    // -- Behaviour the goldens don't name -------------------------------------

    #[test]
    fn nulls_show_a_dash_and_never_zero() {
        let d = render(&day_no_final(), DigestPeriod::Daily, T);
        assert!(d.text.contains("- Yield: — (No completed cohorts yet)"));
        assert!(
            d.text
                .contains("- Time to searchable: — (No completed cohorts yet)")
        );
        assert!(d.text.contains("- Searchable torrents: 84,210\n"));
        let p = render(&paused(), DigestPeriod::Weekly, T);
        assert!(p.text.contains("- Searchable torrents: —"));
        assert!(p.text.contains("- Health: Paused\n"));
    }

    #[test]
    fn a_null_delta_prints_nothing() {
        let d = render(&no_compare(), DigestPeriod::Weekly, T);
        assert!(!d.text.contains("change"));
        assert!(!d.text.contains("vs previous"));
        assert!(!d.text.contains('▲') && !d.text.contains('▼'));
        assert!(!d.html.contains('▲') && !d.html.contains('▼'));
        assert!(!d.text.contains("Top movers"));
        assert!(!d.html.contains("Top movers"));
    }

    #[test]
    fn movers_and_arrows() {
        let d = render(&week(), DigestPeriod::Weekly, T);
        assert!(d.text.contains("- video, by group: ▲ +200"), "{}", d.text);
        assert!(d.text.contains("- audio, by group: ▼ −60"), "{}", d.text);
        assert!(
            d.text
                .contains("Biggest losses: No peers 50%, Timed out 30%, Refused 10%")
        );
        assert!(d.html.contains("<span aria-hidden=\"true\">▲</span> +200"));
        // Harvested is approximate; the others are counts.
        assert!(d.text.contains("- Harvested: ≈\u{a0}10,234\n"));
        assert!(d.text.contains("(>99.9% of stored)"));
        assert!(d.text.contains("168 cohorts, 96 final"));
    }

    #[test]
    fn subject_by_period() {
        let w = render(&week(), DigestPeriod::Weekly, T);
        assert_eq!(
            w.subject,
            "Lodestar weekly digest: Oct 15, 2026 – Oct 22, 2026"
        );
        let d = render(&day_no_final(), DigestPeriod::Daily, T);
        assert_eq!(d.subject, "Lodestar daily digest: Oct 22, 2026");
        assert!(
            d.text
                .contains("Open Insights: /insights?range=24h&compare=1")
        );
        assert!(w.html.contains("href=\"/insights?range=7d&amp;compare=1\""));
        assert!(w.text.contains("Period: Oct 15, 2026, 15:00 – Oct 22, 2026, 15:00 UTC; generated Oct 22, 2026, 14:05 UTC"));
    }

    #[test]
    fn exception_lines_and_links() {
        let d = render(&exceptions(), DigestPeriod::Weekly, T);
        let t = &d.text;
        assert!(t.contains("- Warning: The search index is 42 sec behind. Fix: /insights/index\n"));
        assert!(t.contains(
            "- Error: Nothing was stored for 2 hours while 1,200 torrents are in progress. Fix: /settings/crawler\n"
        ));
        // A target that fails the check has no link, and no path in the text.
        assert!(t.contains("below the 7 days before them.\n"));
        assert!(t.contains("Yield is 21.0% for the newest"));
        assert!(!t.contains("Fetch/"));
        assert!(!d.html.contains("Fetch/"));
        // An unknown kind: a generic line; an unknown unit adds no number.
        assert!(
            t.contains("- Warning: Needs attention: disk_projection. Fix: /insights/storage\n")
        );
        assert!(t.contains("- Warning: Needs attention: mystery.\n"));
        // An unknown fix kind has no link either.
        assert!(!t.contains("/page/"));
        assert_eq!(d.html.matches("<a href=").count(), 4); // three fixes and Insights
    }

    // -- Hostile inputs ------------------------------------------------------

    const HOSTILE: [&str; 6] = [
        "<script>alert(1)</script>",
        "\"><img src=x onerror=alert(1)>",
        "javascript:alert(1)",
        "\u{202e}evil\u{2066}\u{200f}\u{061c}",
        "a\u{0}b\u{1b}[31m\u{85}\u{7f}\r\nHeader: x",
        "' onmouseover='x' style='color:red",
    ];

    fn hostile() -> OverviewResponse {
        let mut o = week();
        let rows: Vec<SegmentCount> = HOSTILE
            .iter()
            .enumerate()
            .map(|(i, k)| row(k, 5_000 + i as u64))
            .collect();
        o.segments.by_group = rows.clone();
        o.segments.by_content_type = rows;
        o.exceptions = HOSTILE
            .iter()
            .map(|k| exception(k, Some("s"), Some(1.0), fix("stage", k)))
            .collect();
        o.kpi.health.reason = Some(HOSTILE[0].into());
        o.funnel_total
            .loss_share
            .insert(HOSTILE[1].into(), Some(0.9));
        o
    }

    /// What no digest may contain, whatever the inputs.
    fn assert_inert(d: &Digest) {
        let html = &d.html;
        // `javascript:` may appear as text (a hostile key), never as a link:
        // the href check below sees every link.
        for bad in [
            "<script",
            "<style",
            "<img",
            "<iframe",
            "<link",
            "<a href=\"j",
            "//",
        ] {
            assert!(!html.to_lowercase().contains(bad), "{bad} in html");
        }
        // No attribute but the ones we write.
        let mut rest = html.as_str();
        while let Some(i) = rest.find('<') {
            let tag = &rest[i..rest[i..].find('>').map_or(rest.len(), |e| i + e + 1)];
            rest = &rest[i + tag.len().max(1)..];
            let lower = tag.to_lowercase();
            assert!(!lower.contains(" on"), "an event handler: {tag}");
            if let Some(s) = tag.find("style=") {
                assert!(tag[s..].starts_with("style=\"text-align:right\""), "{tag}");
            }
            if let Some(h) = tag.find("href=") {
                let v = &tag[h + 6..];
                let v = &v[..v.find('"').unwrap()];
                let ok = v == "/insights?range=24h&amp;compare=1"
                    || v == "/insights?range=7d&amp;compare=1"
                    || (v.starts_with("/insights/") || v.starts_with("/settings/"))
                        && valid_target(v.rsplit('/').next().unwrap());
                assert!(ok, "href {v:?}");
            }
        }
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<html lang=\"en\">") && html.contains("<meta charset=\"utf-8\">"));
        for c in d.text.chars().chain(d.subject.chars()) {
            assert!(
                c == '\n' || !c.is_control(),
                "a control character in the text: {c:?}"
            );
            assert!(
                !matches!(c, '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'),
                "a bidi character: {c:?}"
            );
        }
        for c in html.chars() {
            assert!(c == '\n' || !c.is_control(), "a control in the html: {c:?}");
        }
        assert!(!d.text.contains('\r'));
    }

    #[test]
    fn hostile_strings_are_inert_in_text_and_html() {
        let d = render(&hostile(), DigestPeriod::Weekly, T);
        assert_inert(&d);
        // The script text is there as text, escaped.
        assert!(d.html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(
            d.html
                .contains("&quot;&gt;&lt;img src=x onerror=alert(1)&gt;")
        );
        // No link came from a hostile target.
        assert_eq!(d.html.matches("<a href=").count(), 1);
        // Controls and bidi controls are gone from the text.
        assert!(d.text.contains("evil"));
        assert!(d.text.contains("ab[31m"));
    }

    #[test]
    fn well_formed_digests_are_inert_too() {
        for (o, p) in [
            (week(), DigestPeriod::Weekly),
            (exceptions(), DigestPeriod::Weekly),
            (paused(), DigestPeriod::Weekly),
            (day_no_final(), DigestPeriod::Daily),
            (no_compare(), DigestPeriod::Weekly),
        ] {
            assert_inert(&render(&o, p, T));
        }
    }

    // -- Limits ----------------------------------------------------------------

    #[test]
    fn maximal_inputs_stay_inside_the_limits() {
        let key = "😀\"'&<".repeat(40);
        let mut o = week();
        let rows: Vec<SegmentCount> = (0..50)
            .map(|i| row(&format!("{key}{i}"), 9_000 + i))
            .collect();
        o.segments.by_group = rows.clone();
        o.segments.by_content_type = rows;
        o.exceptions = (0..40)
            .map(|i| {
                exception(
                    &format!("{key}{i}"),
                    Some("s"),
                    Some(f64::from(i) * 1e9),
                    fix("settings", "abcdefghijklmnopqrstuvwxyz_abcdef"),
                )
            })
            .collect();
        o.kpi.health.reason = Some(key.clone());
        let d = render(&o, DigestPeriod::Weekly, T);
        assert!(d.text.len() <= MAX_TEXT_BYTES, "{} bytes", d.text.len());
        assert!(d.html.len() <= MAX_HTML_BYTES, "{} bytes", d.html.len());
        assert!(d.subject.chars().count() <= MAX_SUBJECT_CHARS);
        assert!(d.text.contains("more in Insights"));
        assert_inert(&d);
        // The HTML lists them all; every kind is cut to 64 characters.
        assert_eq!(d.html.matches("Needs attention: ").count(), 40);
    }

    #[test]
    fn every_known_kind_fits_in_the_text() {
        let mut o = week();
        o.exceptions = ["index_lag", "cohort_stalled", "yield_below_baseline"]
            .into_iter()
            .map(|k| exception(k, Some("s"), Some(86_400.0 * 400.0), fix("stage", "fetch")))
            .collect();
        let d = render(&o, DigestPeriod::Weekly, T);
        assert!(d.text.len() < MAX_TEXT_BYTES / 2);
        assert!(!d.text.contains("more in Insights"));
    }
}
