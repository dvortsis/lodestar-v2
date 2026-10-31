//! The filters API (architecture §20.7, §20.10–§20.12): the rule set,
//! the review queue, and changes to filtering.
//!
//! Every change goes through one protocol:
//! - `POST /filters/preview` returns what the change would do: an impact
//!   preview and, for a change that weakens protection, a plain-language
//!   effect warning. It never applies anything and never asks for the
//!   password.
//! - `POST /filters/apply` applies it. A weakening change is refused with
//!   428 and the same warning until it's sent again with the admin
//!   password, every time (§20.11). Strengthening changes need neither.
//!
//! No change is recorded anywhere: no author, reason or history. The one
//! previous rule set is kept for Undo (§20.5.6). The guardrails (the
//! locked floor, protective core actions, valid overrides and exceptions)
//! hold on this server whatever a client sends.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use lodestar_classify::ContentType;
use lodestar_filter::lists::SharedIps;
use lodestar_filter::load::{KIND_HASHES, KIND_KEYWORD, KIND_PAYLOAD, KIND_QUERY, encode_match};
use lodestar_filter::{
    Action, CoreRule, Doc, Engine, Exception, Facts, Fixture, Handle, HeldSet, Item,
    MAX_CLASS_RULES, Match, Origin, Override, Pack, PackRule, RuleRef, Sealer, Strength,
};
use lodestar_search::{Node, parse};
use lodestar_store::{ExceptionRow, FloorSource, OverrideRow, RuleRow, RuleSet, Store};
use serde_json::{Value, json};
use utoipa::OpenApi;

use crate::ApiState;
use crate::auth::{self, auth_error};
use crate::dto::auth::OkResponse;
use crate::dto::filters::{
    AllowEntry, ApplyResponse, ClassRule, ClassRuleAdded, ClassRuleRemoved, Classes, CoreRuleState,
    CorrectResponse, Decision, DecisionCount, Detection, DetectionExample, ExceptionEntry,
    FixtureCounts, Fixtures, JobPhase, JobProgress, JobStarted, JobState, JobStatus, OverrideEntry,
    Overview, PackRuleState, PackState, PendingFloorEntry, QuarantineEntry, Queue, QueueQuery,
    RuleEntry, Strictness, StrictnessList, SubscriptionEntry, TestResponse, TestResult,
    TombstoneEntry, Tombstones, TombstonesQuery,
};
pub use crate::dto::filters::{
    Assessment, Change, ChangeBody, ClassRuleBody, CorrectBody, IdBody, MatchInput, Preview,
    Sample, SampleFile, StrictnessBody, TestBody,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{ClientIp, Json as JsonBody, Path, Query, same_origin_json};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(
    overview,
    queue,
    tombstones,
    fixtures,
    test,
    strictness,
    classes,
    correct,
    add_class_rule,
    remove_class_rule,
    preview,
    start_preview,
    preview_job,
    cancel_preview,
    apply
))]
pub(crate) struct ApiDoc;

/// What the filters API works with. The store is the writable control
/// connection, not the ingest writer's.
#[derive(Debug)]
pub struct FiltersApi {
    pub store: Arc<Mutex<Store>>,
    pub engine: Arc<Handle>,
    pub held: Arc<HeldSet>,
    pub sealer: Option<Arc<Sealer>>,
    pub ips: Arc<SharedIps>,
    /// For finding the stored torrents a classification rule matches.
    pub index: Option<lodestar_search::SearchHandle>,
    /// Starts another background-enrichment pass in the pipeline.
    pub reenrich: Arc<AtomicBool>,
    pub jobs: PreviewJobs,
    /// How long a quarantine lasts (§20.3), for a torrent a correction
    /// relabels adult and filtering then holds back.
    pub quarantine_secs: i64,
    /// The server's shutdown, the only thing that stops a re-queue: a
    /// deadline never does, or a new rule's existing matches would stay
    /// unmarked. A re-queue cut short stays pending and runs again at the
    /// next start ([`FiltersApi::resume_requeues`]).
    pub shutdown: lodestar_search::CancelToken,
}

/// The replay window (§20.10).
const REPLAY_SECS: i64 = 30 * 86_400;
/// Rows read for a preview.
const PREVIEW_ROWS: usize = 100_000;
/// Quarantine rows per review-queue page, at most and by default.
const QUEUE_PAGE: usize = 1_000;
/// Hashes listed per source of pending floor removals.
const PENDING_HASHES: usize = 1_000;
/// Floor entries one removal settles at most, and a request may name: a
/// larger pending list is removed in batches, each confirmed.
const MAX_FLOOR_BATCH: usize = 10_000;

/// The filters routes (§20.7): all behind a session.
pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/filters", get(overview))
            .route("/filters/queue", get(queue))
            .route("/filters/tombstones", get(tombstones))
            .route("/filters/fixtures", get(fixtures))
            .route("/filters/test", post(test))
            .route("/filters/strictness", post(strictness))
            .route("/classify", get(classes))
            .route("/classify/correct", post(correct))
            .route("/classify/rules", post(add_class_rule))
            .route("/classify/rules/remove", post(remove_class_rule))
            .route("/filters/preview", post(preview))
            .route("/filters/preview/jobs", post(start_preview))
            .route("/filters/preview/jobs/{id}", get(preview_job))
            .route("/filters/preview/jobs/{id}/cancel", post(cancel_preview))
            .route("/filters/apply", post(apply)),
    )
}

/// A 400 for a change or test the filters refuse.
fn bad(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::bad_request(code, message.into())
}

fn not_found(code: &'static str, message: &'static str) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, code, message)
}

fn conflict(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, code, message.into())
}

/// The state changed since the preview so that this change now weakens
/// filtering: it must be previewed (and confirmed) again.
fn changed_since_preview() -> ApiError {
    conflict(
        "changed_since_preview",
        "Filtering changed since this was previewed: preview it again.",
    )
}

fn hex20(s: &str) -> ApiResult<[u8; 20]> {
    let s = s.trim();
    if s.len() != 40 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad(
            "bad_hash",
            format!("{s:?} isn't a 40-digit info hash."),
        ));
    }
    let mut out = [0u8; 20];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex");
    }
    Ok(out)
}

fn hex(h: &[u8]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

fn floor_source(key: &str) -> ApiResult<FloorSource> {
    FloorSource::from_key(key).ok_or_else(|| {
        bad(
            "unknown_floor_source",
            "A floor source is file:<path> or list:<id>.",
        )
        .field("source")
    })
}

/// The pending entries of `source` a floor change names: `hashes`, or all.
fn pending_chosen(
    store: &Store,
    source: &FloorSource,
    hashes: Option<&[String]>,
) -> ApiResult<Vec<[u8; 20]>> {
    if hashes.is_some_and(|hs| hs.len() > MAX_FLOOR_BATCH) {
        return Err(bad(
            "too_many_hashes",
            format!("Settle at most {MAX_FLOOR_BATCH} floor entries at a time."),
        )
        .field("hashes"));
    }
    let pending = store.pending_floor_of(source).map_err(internal)?;
    let chosen: Vec<[u8; 20]> = match hashes {
        Some(hs) => {
            let hs: HashSet<[u8; 20]> = hs.iter().map(|h| hex20(h)).collect::<ApiResult<_>>()?;
            pending.into_iter().filter(|p| hs.contains(p)).collect()
        }
        None => pending,
    };
    if chosen.is_empty() {
        return Err(not_found(
            "floor_removal_not_found",
            "There's no such pending floor removal.",
        ));
    }
    Ok(chosen)
}

fn action(name: &str) -> ApiResult<Action> {
    [
        Action::Flag,
        Action::Demote,
        Action::Hide,
        Action::Quarantine,
        Action::Drop,
    ]
    .into_iter()
    .find(|a| a.as_str() == name)
    .ok_or_else(|| {
        bad(
            "unknown_action",
            "The action must be flag, demote, hide, quarantine or drop.",
        )
        .field("action")
    })
}

fn query(text: &str) -> ApiResult<Node> {
    parse(text).map_err(|e| {
        bad(
            "query_syntax",
            format!("The query doesn't parse: {}", e.message),
        )
        .span(e.start, e.end)
    })
}

fn days(d: Option<u32>, now: i64) -> Option<i64> {
    d.map(|d| now + i64::from(d.min(3650)) * 86_400)
}

impl MatchInput {
    fn to_match(&self) -> ApiResult<Match> {
        let given = [
            self.hashes.is_some(),
            self.keyword.is_some(),
            self.query.is_some(),
            self.payload.is_some(),
        ];
        if given.iter().filter(|g| **g).count() != 1 {
            return Err(bad(
                "bad_match",
                "Give exactly one of hashes, keyword, query or payload.",
            ));
        }
        if let Some(hs) = &self.hashes {
            if hs.is_empty() {
                return Err(bad("bad_match", "Give at least one hash.").field("hashes"));
            }
            return Ok(Match::Hashes(
                hs.iter().map(|h| hex20(h)).collect::<ApiResult<_>>()?,
            ));
        }
        if let Some(k) = &self.keyword {
            return Ok(Match::Keyword(k.clone()));
        }
        if let Some(q) = &self.query {
            return Ok(Match::Query(query(q)?));
        }
        let p = self.payload.as_deref().unwrap_or_default();
        u64::from_str_radix(p, 16)
            .ok()
            .filter(|_| p.len() == 16)
            .map(Match::Payload)
            .ok_or_else(|| {
                bad("bad_match", "A payload signature is 16 hex digits.").field("payload")
            })
    }
}

/// How far a preview has got (§20.10): shared with a running job, read by
/// `GET /filters/preview/jobs/{id}`.
#[derive(Debug, Default)]
pub struct Progress {
    phase: AtomicU8,
    done: AtomicU64,
    total: AtomicU64,
    cancelled: AtomicBool,
}

const PHASES: [JobPhase; 4] = [
    JobPhase::Reading,
    JobPhase::Quarantine,
    JobPhase::Tombstones,
    JobPhase::Done,
];
const READING: u8 = 0;
const QUARANTINE: u8 = 1;
const TOMBSTONES: u8 = 2;
const DONE: u8 = 3;

impl Progress {
    fn begin(&self, phase: u8, total: usize) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(total as u64, Ordering::Relaxed);
        self.phase.store(phase, Ordering::Release);
    }

    /// Counts `n` more items; refused once the job was cancelled.
    fn step(&self, n: u64) -> ApiResult<()> {
        self.done.fetch_add(n, Ordering::Relaxed);
        if self.cancelled.load(Ordering::Acquire) {
            return Err(conflict("preview_cancelled", "The preview was cancelled."));
        }
        Ok(())
    }

    /// Stops the preview at its next step.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn snapshot(&self) -> JobProgress {
        let phase = self.phase.load(Ordering::Acquire) as usize;
        JobProgress {
            done: self.done.load(Ordering::Relaxed),
            phase: PHASES[phase.min(PHASES.len() - 1)],
            total: self.total.load(Ordering::Relaxed),
        }
    }
}

/// A preview running in the background (§20.10), so a large one never
/// holds a request open and can be cancelled.
#[derive(Debug, Default)]
pub struct Job {
    progress: Progress,
    result: Mutex<Option<ApiResult<Assessment>>>,
    finished: Mutex<Option<Instant>>,
}

impl Job {
    fn finish(&self, result: ApiResult<Assessment>) {
        *self.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        *self.finished.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }

    /// What `GET /filters/preview/jobs/{id}` reports.
    fn status(&self) -> JobStatus {
        let result = self.result.lock().unwrap_or_else(|e| e.into_inner());
        let cancelled = self.progress.cancelled.load(Ordering::Acquire);
        let (state, assessment, error) = match result.as_ref() {
            None => (JobState::Running, None, None),
            Some(Ok(a)) => (JobState::Done, Some(a.clone()), None),
            Some(Err(_)) if cancelled => (JobState::Cancelled, None, None),
            Some(Err(e)) => (JobState::Failed, None, Some(e)),
        };
        JobStatus {
            assessment,
            error: error.map(ApiError::error_body),
            progress: self.progress.snapshot(),
            state,
            status: error.map(|e| e.status().as_u16()),
        }
    }

    fn finished_at(&self) -> Option<Instant> {
        *self.finished.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs the preview `f` for this job. If it panics, the job still
    /// finishes, with a 500, so it never holds a running slot for good.
    fn run(&self, f: impl FnOnce(&Progress) -> ApiResult<Assessment>) {
        struct Guard<'a>(&'a Job);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                if self.0.finished_at().is_none() {
                    self.0
                        .finish(Err(internal("a filter preview job panicked")));
                }
            }
        }
        let guard = Guard(self);
        let result = f(&self.progress);
        guard.0.finish(result);
    }
}

/// The preview jobs of this server; finished ones are kept for
/// [`JOB_KEEP`], at most [`MAX_RETAINED`] of them.
#[derive(Debug, Default)]
pub struct PreviewJobs {
    next: AtomicU64,
    jobs: Mutex<HashMap<u64, Arc<Job>>>,
}

/// Previews running at once, at most.
const MAX_JOBS: usize = 4;
/// Jobs kept at most, running ones included; the oldest finished go first.
const MAX_RETAINED: usize = 64;
/// How long a finished preview's result is kept.
const JOB_KEEP: Duration = Duration::from_secs(600);

impl PreviewJobs {
    fn jobs(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Arc<Job>>> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.retain(|_, j| j.finished_at().is_none_or(|at| at.elapsed() < JOB_KEEP));
        jobs
    }

    fn start(&self) -> ApiResult<(u64, Arc<Job>)> {
        let mut jobs = self.jobs();
        let running = jobs.values().filter(|j| j.finished_at().is_none()).count();
        if running >= MAX_JOBS {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_previews",
                "Too many previews are running: wait for one to finish, or cancel it.",
            ));
        }
        while jobs.len() >= MAX_RETAINED {
            let oldest = jobs
                .iter()
                .filter_map(|(id, j)| j.finished_at().map(|at| (at, *id)))
                .min();
            match oldest {
                Some((_, id)) => jobs.remove(&id),
                None => break,
            };
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let job = Arc::new(Job::default());
        jobs.insert(id, job.clone());
        Ok((id, job))
    }

    fn get(&self, id: u64) -> ApiResult<Arc<Job>> {
        self.jobs()
            .get(&id)
            .cloned()
            .ok_or_else(|| not_found("preview_not_found", "There's no such preview."))
    }
}

/// The current state, read once per preview.
struct Snapshot {
    /// Every rule's held hashes: `(quarantined, tombstoned)`, counted in
    /// full (the rows below are the newest only).
    held: HashMap<String, (usize, usize)>,
    quarantine: Vec<lodestar_store::Quarantined>,
    tombstones: Vec<lodestar_store::Tombstone>,
    hits: Vec<(String, u8, i64)>,
}

impl Snapshot {
    fn read(store: &Store, now: i64) -> lodestar_store::Result<Snapshot> {
        Ok(Snapshot {
            held: store
                .held_counts()?
                .into_iter()
                .map(|(r, q, t)| (r, (q, t)))
                .collect(),
            quarantine: store.quarantined(PREVIEW_ROWS)?,
            tombstones: store.tombstones(PREVIEW_ROWS)?,
            hits: store.hit_counts(now - REPLAY_SECS)?,
        })
    }

    /// The preview for changes scoped to whole rules.
    fn for_rules(&self, rules: &[String]) -> Preview {
        let hit = |r: &str| rules.iter().any(|x| x == r);
        let core = |r: &str| CoreRule::from_id(r).is_some();
        let mark = |a: u8| {
            [Action::Hide, Action::Demote, Action::Flag]
                .iter()
                .any(|m| m.code() == a)
        };
        let live: Vec<&(String, u8, i64)> = self
            .hits
            .iter()
            .filter(|(r, a, _)| hit(r) && a & lodestar_store::SHADOW == 0)
            .collect();
        let caught: i64 = live.iter().map(|(_, _, n)| n).sum();
        Preview {
            quarantine: rules
                .iter()
                .filter_map(|r| self.held.get(r))
                .map(|h| h.0)
                .sum(),
            tombstones: rules
                .iter()
                .filter_map(|r| self.held.get(r))
                .map(|h| h.1)
                .sum(),
            stored: live
                .iter()
                .filter(|(_, a, _)| mark(*a))
                .map(|(_, _, n)| *n as u64)
                .sum(),
            caught_30d: caught,
            core_caught_30d: live
                .iter()
                .filter(|(r, _, _)| core(r))
                .map(|(_, _, n)| n)
                .sum(),
            per_day: caught as f64 / 30.0,
            estimate: false,
            removed_from_floor: 0,
        }
    }

    /// Items the `before` engine holds and `after` doesn't, judged from
    /// their sealed names; exact for hash matches.
    fn rejudged(
        &self,
        before: &Engine,
        after: &Engine,
        sealer: Option<&Sealer>,
        now: i64,
        progress: &Progress,
    ) -> ApiResult<Preview> {
        let mut p = Preview {
            estimate: true,
            ..Preview::default()
        };
        progress.begin(QUARANTINE, self.quarantine.len());
        for q in &self.quarantine {
            progress.step(1)?;
            let name = sealer
                .zip(q.sealed.as_deref())
                .and_then(|(s, sealed)| s.open(sealed).ok())
                .unwrap_or_default();
            let hashes = [q.hash];
            let item = Item {
                hashes: &hashes,
                name: &name,
                files: &[],
                content_type: ContentType::Other,
            };
            let (b, a) = (
                before.judge(&item, None, now).decision,
                after.judge(&item, None, now).decision,
            );
            let released = match (b, a) {
                (Some(_), None) => true,
                // Held by evidence a name can't show (files): released if
                // the new rules let its exact hash through.
                (None, None) => {
                    let blank = Item { name: "", ..item };
                    after.judge(&blank, None, now).decision.is_none()
                        && released_by_hash(after, &q.hash, &q.rule)
                }
                _ => false,
            };
            if released {
                p.quarantine += 1;
            }
        }
        progress.begin(TOMBSTONES, self.tombstones.len());
        for chunk in self.tombstones.chunks(1024) {
            p.tombstones += chunk
                .iter()
                .filter(|t| released_by_hash(after, &t.hash, &t.rule))
                .count();
            progress.step(chunk.len() as u64)?;
        }
        Ok(p)
    }
}

/// Whether a hash-level entry (override or exception) in `engine` covers
/// this hash for this rule.
fn released_by_hash(engine: &Engine, hash: &[u8; 20], rule: &str) -> bool {
    let covers = |m: &Match| matches!(m, Match::Hashes(hs) if hs.contains(hash));
    let floor = CoreRule::from_id(rule).is_some_and(|r| r.floor());
    if floor {
        return false;
    }
    engine
        .overrides
        .iter()
        .any(|o| o.strength == Strength::Override && covers(&o.what))
        || engine
            .exceptions
            .iter()
            .any(|e| e.rule.id() == rule && covers(&e.what))
}

/// A review-queue cursor, `{at}.{hash}`: the last row of the page before.
fn queue_cursor(c: &str) -> ApiResult<(i64, [u8; 20])> {
    let bad_cursor = || bad("bad_cursor", "That isn't a review-queue cursor.").field("cursor");
    let (at, hash) = c.split_once('.').ok_or_else(bad_cursor)?;
    let at = at.parse().map_err(|_| bad_cursor())?;
    let hash = hex20(hash).map_err(|_| bad_cursor())?;
    Ok((at, hash))
}

fn pack_rules(id: &str) -> ApiResult<Vec<String>> {
    if let Some(p) = Pack::from_name(id) {
        return Ok(PackRule::ALL
            .into_iter()
            .filter(|r| r.pack() == p)
            .map(|r| r.id().to_owned())
            .collect());
    }
    PackRule::from_id(id)
        .map(|r| vec![r.id().to_owned()])
        .ok_or_else(|| {
            bad(
                "unknown_pack",
                format!("There's no pack or pack rule {id:?}."),
            )
        })
}

fn override_entry(
    what: &MatchInput,
    neutralize: bool,
    scope: Option<&str>,
    expires_days: Option<u32>,
    shadow_days: Option<u32>,
    now: i64,
) -> ApiResult<Override> {
    let o = Override {
        id: 0,
        what: what.to_match()?,
        strength: if neutralize {
            Strength::Neutralize
        } else {
            Strength::Override
        },
        scope: scope.map(query).transpose()?,
        expires_at: days(expires_days, now),
        shadow_until: days(shadow_days, now),
    };
    o.validate()
        .map_err(|e| bad("invalid_override", e.to_string()))?;
    Ok(o)
}

fn exception_entry(rule: &str, what: &MatchInput) -> ApiResult<Exception> {
    let rule = RuleRef::from_id(rule)
        .ok_or_else(|| bad("unknown_rule", format!("There's no rule {rule:?}.")).field("rule"))?;
    let e = Exception {
        id: 0,
        rule,
        what: what.to_match()?,
    };
    e.validate()
        .map_err(|e| bad("invalid_exception", e.to_string()))?;
    Ok(e)
}

fn count_line(p: &Preview) -> String {
    let core = if p.core_caught_30d > 0 {
        format!(" ({} caught by the child-safety core)", p.core_caught_30d)
    } else {
        String::new()
    };
    let mut s = format!(
        "In the last 30 days the rules it affects decided {} items{core}, about {:.1} per day.",
        p.caught_30d, p.per_day
    );
    if p.quarantine + p.tombstones > 0 {
        s.push_str(&format!(
            " {} items in quarantine would be released and {} dropped items could be fetched again.",
            p.quarantine, p.tombstones
        ));
    }
    if p.stored > 0 {
        s.push_str(&format!(
            " Up to {} stored items would lose their hide, demote or flag mark.",
            p.stored
        ));
    }
    if p.estimate {
        s.push_str(" Counts come from names only, so treat them as estimates.");
    }
    s
}

/// The side effect of releasing an adult hold (§20.11): the relabel that
/// made the Adult-only checks apply may not be there when it's next seen.
const ADULT_HOLD_NOTE: &str = "It was held because your correction or an Is adult \
    classification rule made it adult. A correction is deleted with the torrent: if no Is \
    adult rule matches it when it's next seen, it's judged as detection classifies it, and if \
    that isn't adult, the Adult-only child-safety checks won't look at it.";

/// The effect warning (§20.11): what it does, what else it lets
/// through, the numbers, and a safer alternative.
fn warning(change: &Change, p: &Preview, neutral: Option<&Preview>, adult_hold: bool) -> String {
    let n = count_line(p);
    match change {
        Change::CoreSwitch { rule, .. } => format!(
            "Turning off {rule} stops that child-safety check for every torrent. Other core \
             rules keep checking. {n} Safer: add an exception for the exact torrents it got \
             wrong, which keeps {rule} on for everything else."
        ),
        Change::PackSwitch { id, .. } => format!(
            "Switching off {id} stops it hiding or dropping malware, lures, fakes, spam or scams \
             it would have caught. {n} Safer: an exception for the torrents it got wrong."
        ),
        Change::UpdateRule { id, .. } | Change::RemoveRule { id } => format!(
            "This changes or removes your rule U-{id}, so it may stop catching what it caught. \
             {n} Safer: shadow the new version for a few days first."
        ),
        Change::AddOverride {
            what, neutralize, ..
        } => {
            let thing = what
                .keyword
                .as_ref()
                .map(|k| format!("\"{k}\""))
                .unwrap_or_else(|| "these torrents".into());
            if *neutralize {
                format!(
                    "Neutralizing {thing} stops the word from counting as evidence in every \
                     core rule that isn't locked and in the packs. Other evidence still counts. {n}"
                )
            } else {
                let safer = neutral.map_or_else(String::new, |q| {
                    format!(
                        " Safer: \"Neutralize term\" stops the word counting on its own while \
                         other signals still count; it would release {} items now.",
                        q.quarantine
                    )
                });
                format!(
                    "Overriding {thing} lets matching torrents skip every core rule that isn't \
                     locked, including the ones that detect explicit under-18 ages and code terms \
                     in file names, plus the packs and your rules. Abuse uploads can use the same \
                     words, so those would get through too. {n}{safer}"
                )
            }
        }
        Change::AddException { rule, .. } => format!(
            "This exception releases what it matches from {rule} only. Other rules still check \
             it. {n} Safer: the narrowest match, an exact torrent."
        ),
        Change::AddAllow { .. } => format!(
            "Everything this matches will skip your rules and the rule packs (malware, lures, \
             fakes, spam, scams). The child-safety core still applies. {n} Safer: narrow it, \
             for example to one media type."
        ),
        Change::RemoveCs1 { .. } => format!(
            "You confirmed these torrents as abuse material. Removing them lets them, and exact \
             reposts of their files, be fetched and indexed again. {n}"
        ),
        Change::FalsePositive { .. } => format!(
            "This releases one torrent from the rule that held it, and from that rule only. \
             Other rules still check it, and future torrents with similar names are still \
             checked. It will be fetched and indexed. {n}{}",
            if adult_hold {
                format!(" {ADULT_HOLD_NOTE}")
            } else {
                String::new()
            }
        ),
        Change::Release { .. } if adult_hold => {
            format!(
                "Releasing this torrent fetches and indexes it the next time it's seen. {ADULT_HOLD_NOTE} Safer: keep it held."
            )
        }
        Change::Release { .. } => {
            "Releasing this torrent fetches and indexes it normally the next time it's seen. \
             Every rule checks it again then."
                .to_owned()
        }
        Change::RevealName { .. } => {
            "This shows the name of a torrent a filter held back. Nothing about it is stored."
                .to_owned()
        }
        Change::Unsubscribe { .. } | Change::PauseSubscription { .. } => format!(
            "This stops applying the list: what it blocked can be fetched or contacted again. {n}"
        ),
        Change::ResolveHeld { .. } => format!(
            "This applies a version the sanity gates held back because it shrank sharply: \
             entries it no longer has stop being blocked. {n}"
        ),
        Change::Subscribe { .. } => {
            "Marking this list as part of the locked floor blocks its torrents beyond any \
             override, and you can't release them while they're on it. Only do this for a \
             vetted child-abuse hash list."
                .to_owned()
        }
        Change::ResolveFloorRemoval { source, .. } => {
            let from = match FloorSource::from_key(source) {
                Some(FloorSource::File(path)) => format!("{path} no longer lists them"),
                Some(FloorSource::List(id)) => {
                    format!("the publisher of list {id} removed them")
                }
                None => "their source dropped them".to_owned(),
            };
            format!(
                "This removes {} entries from the locked floor, so those torrents can be fetched \
                 and indexed again. {from}, but a config file or a list can only add to the \
                 floor, so they stay blocked until you remove them here. A list's publisher can \
                 make mistakes or be compromised, and a file can be edited by anyone who can \
                 write to it: remove them only if you know why they were dropped. {} dropped \
                 items could be fetched again. Safer: keep them, which stops this notice and \
                 keeps them blocked.",
                p.removed_from_floor, p.tombstones
            )
        }
        Change::Undo => {
            "Undo puts back the previous rule set: changes made since are reverted, including \
             any that strengthened filtering."
                .to_owned()
        }
        _ => n,
    }
}

impl FiltersApi {
    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// What `change` would do, without doing it.
    pub fn assess(&self, change: &Change, now: i64) -> ApiResult<Assessment> {
        self.assess_tracked(change, now, &Progress::default())
    }

    /// [`FiltersApi::assess`], reporting progress. The store is locked
    /// only while the snapshot is read.
    pub fn assess_tracked(
        &self,
        change: &Change,
        now: i64,
        progress: &Progress,
    ) -> ApiResult<Assessment> {
        progress.begin(READING, 0);
        let snap = Snapshot::read(&self.store(), now).map_err(internal)?;
        let engine = self.engine.current();
        let sealer = self.sealer.as_deref();
        let mut a = Assessment::default();
        let rules = |ids: Vec<String>| snap.for_rules(&ids);
        match change {
            Change::CoreSwitch { rule, on } => {
                let r = CoreRule::from_id(rule).ok_or_else(|| {
                    bad("unknown_rule", format!("There's no rule {rule:?}.")).field("rule")
                })?;
                if r.floor() && !on {
                    return Err(conflict(
                        "floor_locked",
                        format!("{rule} is part of the locked floor and can't be switched off."),
                    ));
                }
                // As stored: the engine can lag the store after an apply
                // that failed part-way.
                let stored_on = self.store().core_switch(r.id()).map_err(internal)?;
                if !on && stored_on {
                    a.weakening = true;
                    a.preview = rules(vec![rule.clone()]);
                }
            }
            Change::PackSwitch { id, on } => {
                let ids = pack_rules(id)?;
                if !on {
                    a.weakening = true;
                    a.preview = rules(ids);
                }
            }
            Change::AddRule {
                query: q,
                action: act,
                unless,
                ..
            } => {
                query(q)?;
                action(act)?;
                unless.as_deref().map(query).transpose()?;
            }
            Change::UpdateRule {
                id,
                enabled,
                query: q,
                action: act,
                unless,
                shadow_days,
            } => {
                q.as_deref().map(query).transpose()?;
                act.as_deref().map(action).transpose()?;
                unless
                    .as_deref()
                    .filter(|u| !u.is_empty())
                    .map(query)
                    .transpose()?;
                let only_enabling = *enabled == Some(true)
                    && q.is_none()
                    && act.is_none()
                    && unless.is_none()
                    && shadow_days.is_none();
                if !only_enabling {
                    a.weakening = true;
                    a.preview = rules(vec![format!("U-{id}")]);
                }
            }
            Change::RemoveRule { id } => {
                a.weakening = true;
                a.preview = rules(vec![format!("U-{id}")]);
            }
            Change::AddOverride {
                what,
                neutralize,
                scope,
                expires_days,
                shadow_days,
            } => {
                let o = override_entry(
                    what,
                    *neutralize,
                    scope.as_deref(),
                    *expires_days,
                    *shadow_days,
                    now,
                )?;
                a.weakening = true;
                let mut after = Engine::clone(&engine);
                after.overrides.push(Override {
                    id: u32::MAX,
                    ..o.clone()
                });
                a.preview = snap.rejudged(&engine, &after, sealer, now, progress)?;
                // Every non-floor rule could be bypassed: the replay's
                // upper bound.
                let bypassable: Vec<String> = snap
                    .hits
                    .iter()
                    .map(|(r, _, _)| r.clone())
                    .filter(|r| !CoreRule::from_id(r).is_some_and(|c| c.floor()))
                    .collect();
                let bound = rules(bypassable);
                a.preview.caught_30d = bound.caught_30d;
                a.preview.core_caught_30d = bound.core_caught_30d;
                a.preview.per_day = bound.per_day;
                if what.keyword.is_some() && !neutralize {
                    let single = Override {
                        id: u32::MAX,
                        strength: Strength::Neutralize,
                        ..o
                    };
                    if single.validate().is_ok() {
                        let mut alt = Engine::clone(&engine);
                        alt.overrides.push(single);
                        a.neutralize = Some(snap.rejudged(&engine, &alt, sealer, now, progress)?);
                    }
                }
            }
            Change::AddException { rule, what } => {
                let e = exception_entry(rule, what)?;
                a.weakening = true;
                let mut after = Engine::clone(&engine);
                after.exceptions.push(Exception { id: u32::MAX, ..e });
                a.preview = snap.rejudged(&engine, &after, sealer, now, progress)?;
                let bound = rules(vec![rule.clone()]);
                a.preview.caught_30d = bound.caught_30d;
                a.preview.core_caught_30d = bound.core_caught_30d;
                a.preview.per_day = bound.per_day;
            }
            Change::AddAllow { query: q } => {
                let node = query(q)?;
                a.weakening = true;
                let mut after = Engine::clone(&engine);
                after.allow.push(lodestar_filter::Allow {
                    id: u32::MAX,
                    query: node,
                });
                a.preview = snap.rejudged(&engine, &after, sealer, now, progress)?;
                let layers: Vec<String> = snap
                    .hits
                    .iter()
                    .map(|(r, _, _)| r.clone())
                    .filter(|r| {
                        matches!(
                            RuleRef::from_id(r),
                            Some(RuleRef::User(_) | RuleRef::Pack(_) | RuleRef::List(_))
                        )
                    })
                    .collect();
                let bound = rules(layers);
                a.preview.caught_30d = bound.caught_30d;
                a.preview.per_day = bound.per_day;
            }
            Change::RemoveCs1 { hashes } => {
                let hs: Vec<[u8; 20]> =
                    hashes.iter().map(|h| hex20(h)).collect::<ApiResult<_>>()?;
                a.weakening = true;
                a.preview.tombstones = snap
                    .tombstones
                    .iter()
                    .filter(|t| hs.contains(&t.hash))
                    .count();
            }
            Change::FalsePositive { hash } => {
                let h = hex20(hash)?;
                let q = snap
                    .quarantine
                    .iter()
                    .find(|q| q.hash == h)
                    .ok_or_else(|| {
                        not_found("not_in_quarantine", "That torrent isn't in quarantine.")
                    })?;
                exception_entry(
                    &q.rule,
                    &MatchInput {
                        hashes: Some(vec![hash.clone()]),
                        ..MatchInput::default()
                    },
                )?;
                a.weakening = true;
                let bound = rules(vec![q.rule.clone()]);
                a.preview = Preview {
                    quarantine: 1,
                    ..bound
                };
            }
            Change::Release { hash } => {
                let h = hex20(hash)?;
                a.weakening = true;
                a.preview.quarantine = snap.quarantine.iter().filter(|q| q.hash == h).count();
                a.preview.tombstones = snap.tombstones.iter().filter(|t| t.hash == h).count();
            }
            Change::RevealName { hash } => {
                hex20(hash)?;
                a.weakening = true;
            }
            Change::Unsubscribe { id } => {
                a.weakening = true;
                a.preview = rules(vec![format!("L-{id}")]);
            }
            Change::PauseSubscription { id, enabled } => {
                if !enabled {
                    a.weakening = true;
                    a.preview = rules(vec![format!("L-{id}")]);
                }
            }
            Change::ResolveHeld { id, accept } => {
                let sub = self
                    .store()
                    .subscription(*id)
                    .map_err(internal)?
                    .ok_or_else(|| {
                        not_found("subscription_not_found", "There's no such subscription.")
                    })?;
                let shrank = sub.error.as_deref().is_some_and(|e| e.contains("shrank"));
                // A floor-marked list's removals wait for a decision of
                // their own (§2.7), so accepting one releases nothing.
                if *accept && shrank && !sub.floor {
                    a.weakening = true;
                    a.preview = rules(vec![format!("L-{id}")]);
                }
            }
            Change::ResolveFloorRemoval {
                source,
                hashes,
                remove,
            } => {
                let mut chosen =
                    pending_chosen(&self.store(), &floor_source(source)?, hashes.as_deref())?;
                // A removal goes in batches the client can name back.
                if *remove {
                    chosen.truncate(MAX_FLOOR_BATCH);
                }
                a.floor_entries = Some(
                    chosen
                        .iter()
                        .take(MAX_FLOOR_BATCH)
                        .map(|h| hex(h))
                        .collect(),
                );
                if *remove {
                    let set: HashSet<&[u8; 20]> = chosen.iter().collect();
                    a.weakening = true;
                    a.preview.tombstones = snap
                        .tombstones
                        .iter()
                        .filter(|t| set.contains(&t.hash))
                        .count();
                    a.preview.removed_from_floor = chosen.len();
                }
                a.settles = Some(chosen);
            }
            Change::Subscribe { floor, kind, .. } => {
                if *floor && kind != "content" {
                    return Err(bad(
                        "floor_needs_content_list",
                        "Only content lists can join the floor.",
                    )
                    .field("floor"));
                }
                a.confirm = *floor;
            }
            Change::Undo => {
                a.weakening = true;
                a.preview.estimate = true;
            }
            Change::RemoveOverride { .. }
            | Change::RemoveException { .. }
            | Change::RemoveAllow { .. }
            | Change::ReportAbuse { .. }
            | Change::Confirm { .. } => {}
        }
        a.confirm |= a.weakening;
        if a.confirm {
            let adult_hold = match change {
                Change::Release { hash } | Change::FalsePositive { hash } => {
                    let h = hex20(hash)?;
                    self.store().adult_hold(&h).map_err(internal)?
                }
                _ => false,
            };
            a.warning = Some(warning(
                change,
                &a.preview,
                a.neutralize.as_ref(),
                adult_hold,
            ));
        }
        progress.begin(DONE, 0);
        Ok(a)
    }

    /// Reloads the rule layers and switches from the store.
    fn reload(&self, store: &Store) -> ApiResult<()> {
        let set = store.rule_set().map_err(internal)?;
        let class_rules = store.class_rules().map_err(internal)?;
        let state = store.filter_state().map_err(internal)?;
        for _note in self.engine.update_engine(|e| {
            for (rule, on) in &state.switches {
                if let Some(r) = CoreRule::from_id(rule) {
                    let _ = e.core.switches.set(r, *on);
                }
            }
            let mut notes = e.load_rules(&set);
            notes.extend(e.load_class_rules(&class_rules));
            notes
        }) {
            // The note names the rule's terms: filter details stay out of
            // the log (§1.8.8) unless their opt-in is on.
            lodestar_log::log_event!(crate::log_events::FILTERS_RULE_SKIPPED);
        }
        Ok(())
    }

    /// The current rule set, kept for one-step Undo once a change to it
    /// has succeeded.
    fn previous_data(&self, store: &Store) -> ApiResult<Vec<u8>> {
        let set = store.rule_set().map_err(internal)?;
        let switches = store.filter_state().map_err(internal)?.switches;
        let data = json!({
            "rules": set.rules.iter().map(|r| json!({
                "id": r.id, "query": r.query, "action": r.action, "enabled": r.enabled,
                "shadow_until": r.shadow_until, "unless": r.unless_query,
            })).collect::<Vec<_>>(),
            "packs": set.pack_switches,
            "overrides": set.overrides.iter().map(|o| json!({
                "id": o.id, "kind": o.kind, "value": hex(&o.value), "strength": o.strength,
                "scope": o.scope, "created_at": o.created_at, "expires_at": o.expires_at,
                "shadow_until": o.shadow_until, "hits": o.hits,
            })).collect::<Vec<_>>(),
            "exceptions": set.exceptions.iter().map(|e| json!({
                "id": e.id, "rule": e.rule, "kind": e.kind, "value": hex(&e.value),
            })).collect::<Vec<_>>(),
            "allow": set.allow,
            "core": switches,
        });
        Ok(data.to_string().into_bytes())
    }

    fn undo(&self, store: &mut Store) -> ApiResult<()> {
        let data = store
            .previous()
            .map_err(internal)?
            .ok_or_else(|| conflict("nothing_to_undo", "There's nothing to undo."))?;
        let v: Value = serde_json::from_slice(&data).map_err(internal)?;
        let unhex = |s: &Value| -> Vec<u8> {
            let s = s.as_str().unwrap_or_default();
            (0..s.len() / 2)
                .filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok())
                .collect()
        };
        let arr = |k: &str| v[k].as_array().cloned().unwrap_or_default();
        let set = RuleSet {
            rules: arr("rules")
                .iter()
                .map(|r| RuleRow {
                    id: r["id"].as_i64().unwrap_or_default(),
                    query: r["query"].as_str().unwrap_or_default().to_owned(),
                    action: r["action"].as_u64().unwrap_or_default() as u8,
                    enabled: r["enabled"].as_bool().unwrap_or(true),
                    shadow_until: r["shadow_until"].as_i64(),
                    unless_query: r["unless"].as_str().map(str::to_owned),
                })
                .collect(),
            pack_switches: arr("packs")
                .iter()
                .filter_map(|p| Some((p[0].as_str()?.to_owned(), p[1].as_bool()?)))
                .collect(),
            overrides: arr("overrides")
                .iter()
                .map(|o| OverrideRow {
                    id: o["id"].as_i64().unwrap_or_default(),
                    kind: o["kind"].as_u64().unwrap_or_default() as u8,
                    value: unhex(&o["value"]),
                    strength: o["strength"].as_u64().unwrap_or_default() as u8,
                    scope: o["scope"].as_str().map(str::to_owned),
                    created_at: o["created_at"].as_i64().unwrap_or_default(),
                    expires_at: o["expires_at"].as_i64(),
                    shadow_until: o["shadow_until"].as_i64(),
                    hits: o["hits"].as_i64().unwrap_or_default(),
                })
                .collect(),
            exceptions: arr("exceptions")
                .iter()
                .map(|e| ExceptionRow {
                    id: e["id"].as_i64().unwrap_or_default(),
                    rule: e["rule"].as_str().unwrap_or_default().to_owned(),
                    kind: e["kind"].as_u64().unwrap_or_default() as u8,
                    value: unhex(&e["value"]),
                })
                .collect(),
            allow: arr("allow")
                .iter()
                .filter_map(|a| Some((a[0].as_i64()?, a[1].as_str()?.to_owned())))
                .collect(),
        };
        // The set being replaced becomes the one Undo goes back to.
        let replaced = self.previous_data(store)?;
        store.replace_rule_set(&set).map_err(internal)?;
        for r in CoreRule::ALL.iter().filter(|r| !r.floor()) {
            let on = arr("core")
                .iter()
                .find(|c| c[0].as_str() == Some(r.id()))
                .and_then(|c| c[1].as_bool())
                .unwrap_or(true);
            store.set_core_switch(r.id(), on).map_err(internal)?;
        }
        store.save_previous(&replaced).map_err(internal)
    }

    fn release(&self, store: &mut Store, hash: [u8; 20]) -> ApiResult<bool> {
        let had = store.release(&hash).map_err(internal)?;
        self.unhold(store, &[hash])?;
        Ok(had)
    }

    /// Tells intake to fetch again those of `hashes` the store no longer
    /// holds, asked under `HeldSet`'s lock: a writer batch that held one
    /// again since the release committed keeps it refused
    /// (`2026-10-31-B-held-exact.md`).
    /// An error after the release committed returns 500 and leaves the
    /// rest refused until the next start: over-strict, never loose.
    fn unhold(&self, store: &Store, hashes: &[[u8; 20]]) -> ApiResult<()> {
        self.held
            .remove_released(hashes, |hs| store.held_among(hs))
            .map_err(internal)
    }

    /// [`release`](Self::release) for many hashes: one transaction and one
    /// WAL scrub in the store, not one per hash.
    fn release_many(&self, store: &mut Store, hashes: &[[u8; 20]]) -> ApiResult<()> {
        store.release_many(hashes).map_err(internal)?;
        self.unhold(store, hashes)
    }

    /// Releases everything `rules` hold, quarantine and tombstones: they
    /// stopped applying. The engine is reloaded from the store first, so
    /// a hash released early can't be held again by a rule that's gone.
    /// Returns how many hashes were released.
    fn release_rules(&self, store: &mut Store, rules: &[String]) -> ApiResult<usize> {
        self.reload(store)?;
        // Intake keeps refusing them until every chunk is in: over-strict
        // for the length of the release, never loose.
        let mut freed = Vec::new();
        // A chunk that failed leaves the ones before it released.
        let n = store.release_rules(rules, |hashes| freed.extend_from_slice(hashes));
        self.unhold(store, &freed)?;
        n.map_err(internal)
    }

    /// Applies `change`, whose `assessment` the caller has already
    /// checked the password for.
    pub fn apply(
        &self,
        change: &Change,
        assessment: Assessment,
        now: i64,
    ) -> ApiResult<ApplyResponse> {
        let mut guard = self.store();
        let store = &mut *guard;
        let rule_layers = matches!(
            change,
            Change::CoreSwitch { .. }
                | Change::PackSwitch { .. }
                | Change::AddRule { .. }
                | Change::UpdateRule { .. }
                | Change::RemoveRule { .. }
                | Change::AddOverride { .. }
                | Change::RemoveOverride { .. }
                | Change::AddException { .. }
                | Change::RemoveException { .. }
                | Change::AddAllow { .. }
                | Change::RemoveAllow { .. }
                | Change::FalsePositive { .. }
        );
        // Read before the change, saved only once it succeeds, so a
        // refused change never replaces what Undo goes back to.
        let previous = if rule_layers {
            Some(self.previous_data(store)?)
        } else {
            None
        };
        let gone = || not_found("entry_not_found", "There's no such entry.");
        let mut out = ApplyResponse {
            applied: true,
            assessment,
            deleted: None,
            exception: None,
            id: None,
            name: None,
            released: None,
            removed: None,
            settled: None,
            version: 0,
        };
        match change {
            Change::CoreSwitch { rule, on } => {
                let r = CoreRule::from_id(rule).ok_or_else(|| {
                    bad("unknown_rule", format!("There's no rule {rule:?}.")).field("rule")
                })?;
                if r.floor() {
                    if !on {
                        return Err(conflict(
                            "floor_locked",
                            "The locked floor can't be switched off.",
                        ));
                    }
                } else {
                    // As stored, under the store lock (the engine can lag
                    // the store after an apply that failed part-way).
                    let was_on = store.core_switch(r.id()).map_err(internal)?;
                    if !on && was_on && !out.assessment.confirm {
                        // Switched on since the preview, which then asked
                        // for no password: preview again.
                        return Err(changed_since_preview());
                    }
                    store.set_core_switch(r.id(), *on).map_err(internal)?;
                    if !on && was_on {
                        // What it holds is released and processed (§20.5.3).
                        out.released = Some(self.release_rules(store, &[r.id().to_owned()])?);
                    }
                }
            }
            Change::PackSwitch { id, on } => {
                let rules = pack_rules(id)?;
                store.set_pack_switch(id, *on).map_err(internal)?;
                if !on {
                    out.released = Some(self.release_rules(store, &rules)?);
                }
            }
            Change::AddRule {
                query: q,
                action: act,
                unless,
                shadow_days,
            } => {
                let q = query(q)?.to_query();
                let unless = unless
                    .as_deref()
                    .map(query)
                    .transpose()?
                    .map(|u| u.to_query());
                let id = store
                    .add_rule(
                        &q,
                        action(act)?.code(),
                        days(*shadow_days, now),
                        unless.as_deref(),
                    )
                    .map_err(internal)?;
                out.id = Some(id);
            }
            Change::UpdateRule {
                id,
                query: q,
                action: act,
                enabled,
                unless,
                shadow_days,
            } => {
                let mut row = store
                    .rule_set()
                    .map_err(internal)?
                    .rules
                    .into_iter()
                    .find(|r| r.id == *id)
                    .ok_or_else(gone)?;
                if let Some(q) = q {
                    row.query = query(q)?.to_query();
                }
                if let Some(a) = act {
                    row.action = action(a)?.code();
                }
                if let Some(e) = enabled {
                    row.enabled = *e;
                }
                if let Some(u) = unless {
                    row.unless_query = if u.is_empty() {
                        None
                    } else {
                        Some(query(u)?.to_query())
                    };
                }
                if shadow_days.is_some() {
                    row.shadow_until = days(*shadow_days, now);
                }
                store.update_rule(&row).map_err(internal)?;
            }
            Change::RemoveRule { id } => {
                if !store.remove_rule(*id).map_err(internal)? {
                    return Err(gone());
                }
                out.released = Some(self.release_rules(store, &[format!("U-{id}")])?);
            }
            Change::AddOverride {
                what,
                neutralize,
                scope,
                expires_days,
                shadow_days,
            } => {
                let o = override_entry(
                    what,
                    *neutralize,
                    scope.as_deref(),
                    *expires_days,
                    *shadow_days,
                    now,
                )?;
                let (kind, value) = encode_match(&o.what);
                let strength = u8::from(o.strength == Strength::Neutralize);
                let id = store
                    .add_override(
                        kind,
                        &value,
                        strength,
                        o.scope.as_ref().map(Node::to_query).as_deref(),
                        now,
                        o.expires_at,
                        o.shadow_until,
                    )
                    .map_err(internal)?;
                out.id = Some(id);
            }
            Change::RemoveOverride { id } => {
                if !store.remove_override(*id).map_err(internal)? {
                    return Err(gone());
                }
            }
            Change::AddException { rule, what } => {
                let e = exception_entry(rule, what)?;
                let (kind, value) = encode_match(&e.what);
                let id = store
                    .add_exception(&e.rule.id(), kind, &value)
                    .map_err(internal)?;
                out.id = Some(id);
            }
            Change::RemoveException { id } => {
                if !store.remove_exception(*id).map_err(internal)? {
                    return Err(gone());
                }
            }
            Change::AddAllow { query: q } => {
                let id = store.add_allow(&query(q)?.to_query()).map_err(internal)?;
                out.id = Some(id);
            }
            Change::RemoveAllow { id } => {
                if !store.remove_allow(*id).map_err(internal)? {
                    return Err(gone());
                }
            }
            Change::ReportAbuse { hashes } => {
                let hs: Vec<[u8; 20]> =
                    hashes.iter().map(|h| hex20(h)).collect::<ApiResult<_>>()?;
                store
                    .add_cs_hashes(&hs, Origin::Reported.code(), now)
                    .map_err(internal)?;
                // Refused from now on, and gone from the store; the
                // indexer drops the documents on its next catch-up.
                self.engine.update(|c| {
                    for h in &hs {
                        c.cs.add_hash(*h, Origin::Reported);
                    }
                });
                out.deleted = Some(store.delete_torrents(&hs).map_err(internal)?.len());
            }
            Change::RemoveCs1 { hashes } => {
                let hs: Vec<[u8; 20]> =
                    hashes.iter().map(|h| hex20(h)).collect::<ApiResult<_>>()?;
                let n = store.remove_cs_hashes(&hs).map_err(internal)?;
                self.engine.update(|c| {
                    for h in &hs {
                        c.cs.remove_hash(h);
                    }
                });
                self.release_many(store, &hs)?;
                out.removed = Some(n);
            }
            Change::FalsePositive { hash } => {
                let h = hex20(hash)?;
                let q = store
                    .quarantined(PREVIEW_ROWS)
                    .map_err(internal)?
                    .into_iter()
                    .find(|q| q.hash == h)
                    .ok_or_else(gone)?;
                let e = exception_entry(
                    &q.rule,
                    &MatchInput {
                        hashes: Some(vec![hash.clone()]),
                        ..MatchInput::default()
                    },
                )?;
                let (kind, value) = encode_match(&e.what);
                out.exception = Some(
                    store
                        .add_exception(&e.rule.id(), kind, &value)
                        .map_err(internal)?,
                );
                // The core's examples are kept as hashed tokens only.
                let name = self
                    .sealer
                    .as_deref()
                    .zip(q.sealed.as_deref())
                    .and_then(|(s, sealed)| s.open(sealed).ok());
                if let (RuleRef::Core(_), Some(name)) = (e.rule, name) {
                    let f = Fixture::of(&name, &[], ContentType::Other);
                    store
                        .add_fixture(&q.rule, false, &f.encode())
                        .map_err(internal)?;
                }
                self.release(store, h)?;
            }
            Change::Release { hash } => {
                if !self.release(store, hex20(hash)?)? {
                    return Err(gone());
                }
            }
            Change::Confirm { hash } => {
                let h = hex20(hash)?;
                let held = store
                    .quarantined(PREVIEW_ROWS)
                    .map_err(internal)?
                    .into_iter()
                    .any(|q| q.hash == h);
                if !held {
                    return Err(gone());
                }
                store
                    .add_cs_hashes(&[h], Origin::Confirmed.code(), now)
                    .map_err(internal)?;
                // Deletes the sealed name with the quarantine row.
                store.release(&h).map_err(internal)?;
                self.engine.update(|c| c.cs.add_hash(h, Origin::Confirmed));
                // A quarantined torrent isn't stored, but another of its
                // swarm hashes may have been.
                out.deleted = Some(store.delete_torrents(&[h]).map_err(internal)?.len());
            }
            Change::RevealName { hash } => {
                let h = hex20(hash)?;
                let q = store
                    .quarantined(PREVIEW_ROWS)
                    .map_err(internal)?
                    .into_iter()
                    .find(|q| q.hash == h)
                    .ok_or_else(gone)?;
                let name = self
                    .sealer
                    .as_deref()
                    .zip(q.sealed.as_deref())
                    .and_then(|(s, sealed)| s.open(sealed).ok())
                    .ok_or_else(|| {
                        ApiError::new(
                            StatusCode::GONE,
                            "no_sealed_name",
                            "No sealed name is kept for that torrent.",
                        )
                    })?;
                out.name = Some(name);
            }
            Change::Subscribe {
                url,
                name,
                kind,
                format,
                action: act,
                floor,
                refresh_hours,
            } => {
                let ip = match kind.as_str() {
                    "ip" => true,
                    "content" => false,
                    _ => {
                        return Err(bad("unknown_list_kind", "A list's kind is content or ip.")
                            .field("kind"));
                    }
                };
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err(
                        bad("bad_list_url", "The list's URL must be http or https.").field("url")
                    );
                }
                let act = act
                    .as_deref()
                    .map(action)
                    .transpose()?
                    .unwrap_or(Action::Drop);
                if !act.protective() {
                    return Err(
                        bad("bad_list_action", "A list's action is drop or quarantine.")
                            .field("action"),
                    );
                }
                if let Some(f) = format {
                    let fm = lodestar_filter::lists::Format::from_name(f).ok_or_else(|| {
                        bad(
                            "unknown_list_format",
                            "The format must be hashes, p2p, dat or cidr.",
                        )
                        .field("format")
                    })?;
                    if fm.is_ip() != ip {
                        return Err(bad(
                            "list_format_mismatch",
                            "That format doesn't fit this kind of list.",
                        )
                        .field("format"));
                    }
                }
                let id = store
                    .add_subscription(&lodestar_store::NewSubscription {
                        url,
                        name: name.as_deref().unwrap_or(url),
                        kind: u8::from(ip),
                        format: format.as_deref(),
                        action: act.code(),
                        floor: *floor && !ip,
                        interval_secs: i64::from(refresh_hours.unwrap_or(24).max(1)) * 3600,
                    })
                    .map_err(internal)?;
                out.id = Some(id);
            }
            Change::Unsubscribe { id } => {
                if !store.remove_subscription(*id).map_err(internal)? {
                    return Err(gone());
                }
                self.release_list(store, *id)?;
            }
            Change::PauseSubscription { id, enabled } => {
                if !store
                    .set_subscription_enabled(*id, *enabled)
                    .map_err(internal)?
                {
                    return Err(gone());
                }
                if !enabled {
                    self.release_list(store, *id)?;
                }
            }
            Change::ResolveHeld { id, accept } => {
                let sub = store.subscription(*id).map_err(internal)?.ok_or_else(|| {
                    not_found("subscription_not_found", "There's no such subscription.")
                })?;
                let shrank = sub.error.as_deref().is_some_and(|e| e.contains("shrank"));
                if *accept && shrank && !sub.floor && !out.assessment.confirm {
                    return Err(changed_since_preview());
                }
                if !store.resolve_held(*id, *accept, now).map_err(internal)? {
                    return Err(conflict("nothing_held", "No version of that list is held."));
                }
            }
            Change::ResolveFloorRemoval {
                source,
                hashes,
                remove,
            } => {
                let src = floor_source(source)?;
                // Without `hashes` (keeping only: a removal names them),
                // what the assessment counted, not whatever is pending by
                // now.
                let chosen = match (hashes, &out.assessment.settles) {
                    (None, Some(counted)) => counted.clone(),
                    _ => pending_chosen(store, &src, hashes.as_deref())?,
                };
                let done = store
                    .resolve_pending_floor(&src, Some(&chosen), *remove, now)
                    .map_err(internal)?;
                // The store holds the floor; the engine follows it, for
                // what actually left it (an entry another file lists
                // stays). A list's own layer is rebuilt below.
                self.engine.update(|c| match (&src, remove) {
                    (FloorSource::File(_), true) => {
                        for h in &done.unblocked {
                            if c.cs.hash(h) == Some(Origin::Imported) {
                                c.cs.remove_hash(h);
                            }
                        }
                    }
                    (FloorSource::List(_), false) => {
                        for h in &done.settled {
                            c.cs.add_hash(*h, Origin::Imported);
                        }
                    }
                    _ => {}
                });
                self.release_many(store, &done.unblocked)?;
                out.settled = Some(done.settled.len());
            }
            Change::Undo => self.undo(store)?,
        }
        if let Some(p) = previous {
            store.save_previous(&p).map_err(internal)?;
        }
        if matches!(
            change,
            Change::Subscribe { .. }
                | Change::Unsubscribe { .. }
                | Change::PauseSubscription { .. }
                | Change::ResolveHeld { .. }
                | Change::ResolveFloorRemoval { .. }
        ) {
            lodestar_filter::lists::apply(store, &self.engine, &self.ips).map_err(internal)?;
        }
        self.reload(store)?;
        out.version = self.engine.current().version;
        let version = lodestar_log::Count(out.version);
        lodestar_log::log_event!(
            crate::log_events::FILTERS_RULE_SET_UPDATED,
            version = version
        );
        lodestar_log::log_event!(
            crate::log_events::FILTERS_CHANGE_DETAIL,
            change = lodestar_log::Detail(&crate::log_events::change_detail(change)),
            version = version
        );
        Ok(out)
    }

    /// A list that stopped applying lets what it holds go. The list layer
    /// is rebuilt first, so nothing released is held by it again.
    fn release_list(&self, store: &mut Store, id: i64) -> ApiResult<()> {
        lodestar_filter::lists::apply(store, &self.engine, &self.ips).map_err(internal)?;
        self.release_rules(store, &[format!("L-{id}")])?;
        Ok(())
    }

    /// The rule set, switches and lists, as the Filters & Safety screens
    /// show them.
    pub fn overview(&self) -> ApiResult<Overview> {
        let engine = self.engine.current();
        let store = self.store();
        let set = store.rule_set().map_err(internal)?;
        let subs = store.subscriptions().map_err(internal)?;
        let pending = store.pending_floor().map_err(internal)?;
        let quarantine = store.quarantined(PREVIEW_ROWS).map_err(internal)?.len();
        let action_name = |code: u8| Action::from_code(code).map(|a| a.as_str().to_string());
        Ok(Overview {
            allow: set
                .allow
                .iter()
                .map(|(id, q)| AllowEntry {
                    id: *id,
                    query: q.clone(),
                })
                .collect(),
            core: CoreRule::ALL
                .iter()
                .map(|r| CoreRuleState {
                    action: r.action().as_str().to_string(),
                    floor: r.floor(),
                    id: r.id().to_string(),
                    on: engine.core.switches.is_on(*r),
                })
                .collect(),
            cs1_entries: engine.core.cs.len(),
            exceptions: set
                .exceptions
                .iter()
                .map(|e| {
                    let m = StoredMatch::of(e.kind, &e.value);
                    ExceptionEntry {
                        hashes: m.hashes,
                        id: e.id,
                        keyword: m.keyword,
                        payload: m.payload,
                        query: m.query,
                        rule: e.rule.clone(),
                    }
                })
                .collect(),
            ip_ranges: self.ips.current().len(),
            overrides: set
                .overrides
                .iter()
                .map(|o| {
                    let m = StoredMatch::of(o.kind, &o.value);
                    OverrideEntry {
                        expires_at: o.expires_at,
                        hashes: m.hashes,
                        id: o.id,
                        keyword: m.keyword,
                        payload: m.payload,
                        query: m.query,
                        scope: o.scope.clone(),
                        shadow_until: o.shadow_until,
                        strength: if o.strength == 1 {
                            "neutralize"
                        } else {
                            "override"
                        }
                        .to_string(),
                    }
                })
                .collect(),
            // Entries a config file or a floor-marked list dropped, still
            // blocked until settled (§2.7).
            pending_floor_removals: pending
                .iter()
                .map(|p| PendingFloorEntry {
                    entries: p.hashes.len(),
                    hashes: p
                        .hashes
                        .iter()
                        .take(PENDING_HASHES)
                        .map(|h| hex(h))
                        .collect(),
                    kind: match p.source {
                        FloorSource::File(_) => "file",
                        FloorSource::List(_) => "list",
                    }
                    .to_string(),
                    name: match &p.source {
                        FloorSource::File(path) => path.clone(),
                        FloorSource::List(id) => subs
                            .iter()
                            .find(|s| s.id == *id)
                            .map_or_else(|| format!("list {id}"), |s| s.name.clone()),
                    },
                    source: p.source.key(),
                })
                .collect(),
            packs: Pack::ALL
                .iter()
                .map(|p| PackState {
                    id: p.as_str().to_string(),
                    on: engine.packs.on.contains(p),
                    rules: PackRule::ALL
                        .iter()
                        .filter(|r| r.pack() == *p)
                        .map(|r| PackRuleState {
                            action: r.action().as_str().to_string(),
                            id: r.id().to_string(),
                            on: engine.packs.is_on(*r),
                        })
                        .collect(),
                })
                .collect(),
            quarantine,
            rules: set
                .rules
                .iter()
                .map(|r| RuleEntry {
                    action: action_name(r.action),
                    enabled: r.enabled,
                    id: r.id,
                    query: r.query.clone(),
                    shadow_until: r.shadow_until,
                    unless: r.unless_query.clone(),
                })
                .collect(),
            subscriptions: subs
                .iter()
                .map(|s| SubscriptionEntry {
                    action: action_name(s.action),
                    checked_at: s.checked_at,
                    enabled: s.enabled,
                    entries: s.entries,
                    error: s.error.clone(),
                    floor: s.floor,
                    format: s.format.clone(),
                    id: s.id,
                    kind: if s.kind == lodestar_filter::lists::KIND_IP {
                        "ip"
                    } else {
                        "content"
                    }
                    .to_string(),
                    name: s.name.clone(),
                    status: s.status.clone(),
                    updated_at: s.updated_at,
                    url: s.url.clone(),
                })
                .collect(),
            version: engine.version,
        })
    }

    /// Tombstones, newest first, never with names (none is kept): a page,
    /// with `adult` only adult holds, so one can be found and released.
    pub fn tombstones(&self, page: &TombstonesQuery) -> ApiResult<Tombstones> {
        let limit = page.limit.unwrap_or(QUEUE_PAGE);
        if !(1..=QUEUE_PAGE).contains(&limit) {
            return Err(bad("bad_limit", "limit must be 1–1000.").field("limit"));
        }
        let before = page.cursor.as_deref().map(queue_cursor).transpose()?;
        let mut t = self
            .store()
            .tombstones_before(before, page.adult, limit + 1)
            .map_err(internal)?;
        let more = t.len() > limit;
        t.truncate(limit);
        Ok(Tombstones {
            next_cursor: t
                .last()
                .filter(|_| more)
                .map(|l| format!("{}.{}", l.at, hex(&l.hash))),
            tombstones: t
                .iter()
                .map(|t| TombstoneEntry {
                    adult_hold: t.adult_hold,
                    at: t.at,
                    hash: hex(&t.hash),
                    rule: t.rule.clone(),
                })
                .collect(),
        })
    }

    /// The review queue (§20.7): quarantine, never with names, and the
    /// replay window's decisions per rule.
    pub fn queue(&self, now: i64, page: &QueueQuery) -> ApiResult<Queue> {
        let limit = page.limit.unwrap_or(QUEUE_PAGE);
        if !(1..=QUEUE_PAGE).contains(&limit) {
            return Err(bad("bad_limit", "limit must be 1–1000.").field("limit"));
        }
        let before = page.cursor.as_deref().map(queue_cursor).transpose()?;
        let store = self.store();
        let mut q = store
            .quarantined_before(before, limit + 1)
            .map_err(internal)?;
        let more = q.len() > limit;
        q.truncate(limit);
        let next_cursor = q
            .last()
            .filter(|_| more)
            .map(|l| format!("{}.{}", l.at, hex(&l.hash)));
        let hits = store.hit_counts(now - REPLAY_SECS).map_err(internal)?;
        Ok(Queue {
            decisions_30d: hits
                .iter()
                .map(|(rule, act, n)| DecisionCount {
                    action: Action::from_code(act & !lodestar_store::SHADOW)
                        .map(|a| a.as_str().to_string()),
                    count: *n,
                    rule: rule.clone(),
                    shadow: act & lodestar_store::SHADOW != 0,
                })
                .collect(),
            next_cursor,
            quarantine: q
                .iter()
                .map(|q| QuarantineEntry {
                    adult_hold: q.adult_hold,
                    at: q.at,
                    expires_at: q.expires_at,
                    hash: hex(&q.hash),
                    rule: q.rule.clone(),
                    sealed: q.sealed.is_some(),
                })
                .collect(),
        })
    }
}

impl FiltersApi {
    /// How strict a candidate set of lists is (owner's decision §2.9):
    /// the IPv4 share it blocks, its IPv6 ranges, the torrents it drops,
    /// what kind of lists they are, and a warning level with its text.
    /// Lists already fetched are measured from their data; catalog lists
    /// not subscribed yet use the catalog's measurement.
    pub fn strictness(&self, body: &StrictnessBody) -> ApiResult<Strictness> {
        use lodestar_filter::strictness::{self, Candidate, Category, Facts};
        let store = self.store();
        let subs = store.subscriptions().map_err(internal)?;
        let mut ids: Vec<i64> = Vec::new();
        let mut candidates = Vec::new();
        for key in &body.catalog {
            let e = strictness::catalog(key).ok_or_else(|| {
                bad("unknown_list", format!("There's no catalog list {key:?}.")).field("catalog")
            })?;
            match subs.iter().find(|s| s.url == e.url) {
                Some(s) => ids.push(s.id),
                None => candidates.push(Candidate::from_catalog(e)),
            }
        }
        ids.extend(&body.subscriptions);
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            let s = subs.iter().find(|s| s.id == id).ok_or_else(|| {
                not_found("subscription_not_found", "There's no such subscription.")
            })?;
            let ip = s.kind == lodestar_filter::lists::KIND_IP;
            let listed = strictness::CATALOG.iter().find(|e| e.url == s.url);
            let categories = match (listed, ip, s.floor) {
                (Some(e), _, _) => e.categories.to_vec(),
                (None, true, _) => vec![Category::Peers],
                (None, false, true) => vec![Category::ChildAbuse],
                (None, false, false) => vec![Category::Content],
            };
            let data = store.subscription_data(id, false).map_err(internal)?;
            let facts = match (data, ip) {
                (Some(d), true) => {
                    lodestar_filter::lists::IpSet::decode(&d).map_or(Facts::Unknown, Facts::Ips)
                }
                (Some(d), false) => {
                    let mut n = (d.len() / 20) as u64;
                    if s.floor {
                        n += store
                            .pending_floor_of(&FloorSource::List(id))
                            .map_err(internal)?
                            .len() as u64;
                    }
                    Facts::Hashes(n)
                }
                (None, _) => listed.map_or(Facts::Unknown, |e| Facts::Measured(e.measured)),
            };
            candidates.push(Candidate {
                name: s.name.clone(),
                ip,
                floor: s.floor && !ip,
                categories,
                facts,
            });
        }
        let a = strictness::assess(&candidates);
        let cats = |c: &[Category]| c.iter().map(|c| c.as_str().to_string()).collect::<Vec<_>>();
        Ok(Strictness {
            categories: cats(&a.categories),
            estimate: a.estimate,
            floor_hashes: a.floor_hashes,
            hashes: a.hashes,
            ipv4_public_share: a.v4_public_share,
            ipv4_share: a.v4_share,
            ipv6_ranges: a.v6_ranges,
            ipv6_share: a.v6_share,
            level: a.level.as_str().to_string(),
            lists: a
                .lists
                .iter()
                .map(|l| StrictnessList {
                    categories: cats(&l.categories),
                    floor: l.floor,
                    hashes: l.hashes,
                    ipv4_public_share: l.v4_public_share,
                    ipv4_share: l.v4_share,
                    ipv6_ranges: l.v6_ranges,
                    kind: if l.ip { "ip" } else { "content" }.to_string(),
                    measured: l.measured,
                    name: l.name.clone(),
                })
                .collect(),
            text: a.text,
            unknown: a.unknown,
        })
    }
}

/// A stored match (kind and value), as the overview lists it: one of the
/// four fields, or none for a kind this server doesn't know.
#[derive(Default)]
struct StoredMatch {
    hashes: Option<Vec<String>>,
    keyword: Option<String>,
    payload: Option<String>,
    query: Option<String>,
}

impl StoredMatch {
    fn of(kind: u8, value: &[u8]) -> StoredMatch {
        let text = || Some(String::from_utf8_lossy(value).into_owned());
        match kind {
            KIND_HASHES => StoredMatch {
                hashes: Some(value.chunks(20).map(hex).collect()),
                ..StoredMatch::default()
            },
            KIND_KEYWORD => StoredMatch {
                keyword: text(),
                ..StoredMatch::default()
            },
            KIND_QUERY => StoredMatch {
                query: text(),
                ..StoredMatch::default()
            },
            KIND_PAYLOAD => StoredMatch {
                payload: Some(hex(value)),
                ..StoredMatch::default()
            },
            _ => StoredMatch::default(),
        }
    }
}

/// Samples per test request.
const MAX_SAMPLES: usize = 50;

/// Corrections compared with detection per `GET /classify`.
const MAX_CORRECTIONS: usize = 10_000;
/// Corrections read per lock by `GET /classify`.
const CORRECTIONS_CHUNK: usize = 500;
/// Stored torrents one classification rule change re-queues at most.
const MAX_REQUEUE: usize = 1_000_000;
/// The `settings` key holding the queries of re-queues not yet finished:
/// a JSON list, one entry per rule change, written before the change and
/// removed once its marks are in.
const REQUEUE_PENDING: &str = "classify.requeue_pending";

fn pending_requeues(store: &Store) -> ApiResult<Vec<String>> {
    let text = store.setting_text(REQUEUE_PENDING).map_err(internal)?;
    // Unreadable (never written so): nothing can be recovered from it,
    // and it mustn't block rule changes, so it's started afresh.
    Ok(text
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default())
}

fn set_pending_requeues(store: &mut Store, list: &[String]) -> ApiResult<()> {
    if list.is_empty() {
        return store
            .update_settings_text(&[], &[REQUEUE_PENDING])
            .map_err(internal);
    }
    let text = serde_json::to_string(list).map_err(internal)?;
    store
        .set_settings_text(&[(REQUEUE_PENDING, &text)])
        .map_err(internal)
}

/// Records a re-queue of `query` as pending, before the rule change it
/// follows, so a stop anywhere after the change leaves it to redo. A query
/// already pending isn't added twice, so while there's no search index to
/// clear them the list holds one entry per query text; [`FiltersApi::requeue`] keeps an
/// entry a later rule change still needs. `true` when it was added.
fn push_pending_requeue(store: &mut Store, query: &str) -> ApiResult<bool> {
    let mut list = pending_requeues(store)?;
    if list.iter().any(|q| q == query) {
        return Ok(false);
    }
    list.push(query.to_owned());
    set_pending_requeues(store, &list)?;
    Ok(true)
}

/// Clears one pending re-queue of `query` once its marks are in.
fn drop_pending_requeue(store: &mut Store, query: &str) -> ApiResult<()> {
    let mut list = pending_requeues(store)?;
    if let Some(i) = list.iter().position(|q| q == query) {
        list.remove(i);
        set_pending_requeues(store, &list)?;
    }
    Ok(())
}

/// A re-queue stopped by the server's shutdown.
fn stopping() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "server_stopping",
        "Lodestar is stopping. The change is saved, and its torrents are queued again at the next start.",
    )
}

/// A classification rule's query: it decides the type, so it can't test
/// the type itself (the index already holds corrected types).
fn class_query(text: &str) -> ApiResult<Node> {
    let node = query(text).map_err(|e| e.field("query"))?;
    if matches!(node, Node::All) {
        return Err(bad(
            "class_rule_needs_query",
            "A classification rule needs a query.",
        )
        .field("query"));
    }
    use lodestar_search::query::TagKey;
    if node.mentions_tag(TagKey::Type) || node.mentions_tag(TagKey::Adult) {
        return Err(bad(
            "class_rule_tests_type",
            "A classification rule decides the type, so its query can't test type: or adult:.",
        )
        .field("query"));
    }
    Ok(node)
}

impl FiltersApi {
    /// The core rules' hashed examples, run against the rules as they are
    /// (§20.8): how many each has and how many it still catches.
    pub fn fixtures(&self) -> ApiResult<Fixtures> {
        let engine = self.engine.current();
        let store = self.store();
        let mut out = Vec::new();
        for r in CoreRule::ALL {
            let (mut must, mut must_not, mut failing) = (0, 0, 0);
            for (_, must_match, tokens) in store.fixtures(r.id()).map_err(internal)? {
                let Some(f) = Fixture::decode(&tokens) else {
                    lodestar_log::log_event!(
                        crate::log_events::FILTERS_FIXTURE_UNREADABLE,
                        rule = r.id()
                    );
                    continue;
                };
                let caught = engine.core.fixture_hits(&f).iter().any(|h| h.rule == r);
                if must_match {
                    must += 1;
                } else {
                    must_not += 1;
                }
                if caught != must_match {
                    failing += 1;
                }
            }
            out.push(FixtureCounts {
                failing,
                must_match: must,
                must_not_match: must_not,
                rule: r.id().to_string(),
            });
        }
        Ok(Fixtures { core: out })
    }

    /// Applies a classification correction (§20.9) and classifies the
    /// torrent again with it, so search sees it at the next index commit.
    pub fn correct(&self, body: &CorrectBody) -> ApiResult<CorrectResponse> {
        let hash = hex20(&body.hash)?;
        let mut store = self.store();
        let id = store
            .find(&lodestar_metainfo::InfoHash(hash))
            .map_err(internal)?
            .ok_or_else(|| not_found("torrent_not_found", "There's no torrent with that hash."))?;
        let fix = lodestar_classify::Correction {
            adult: body.adult,
            not_junk: body.not_junk,
        };
        let stored = lodestar_store::ClassCorrection {
            adult: fix.adult,
            not_junk: fix.not_junk,
        };
        if !store.set_correction(id, stored).map_err(internal)? {
            return Err(not_found(
                "torrent_not_found",
                "There's no torrent with that hash.",
            ));
        }
        let (Some(row), Some(tree)) = (
            store.torrent(id).map_err(internal)?,
            store.files(id).map_err(internal)?,
        ) else {
            return Err(not_found(
                "torrent_not_found",
                "There's no torrent with that hash.",
            ));
        };
        let was_adult = store.content_type(id).map_err(internal)?
            == Some(lodestar_classify::ContentType::Adult.code());
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        // Under the lock, the generation can't move between reading it and
        // the write; the check still guards the order (§20.9).
        let generation = store.class_gen().map_err(internal)?;
        let engine = self.engine.current();
        let now = crate::unix_now();
        // Becoming adult, it's judged as adult too, as at ingest (§20.9).
        let r = lodestar_filter::reclassify::reclassify_row(
            &row,
            &files,
            fix,
            was_adult,
            Some(&engine),
            now,
        );
        let refilings = r.refilings(&row, self.sealer.as_deref());
        let refiled: Vec<_> = refilings
            .iter()
            .map(|f| f.refiled(now, now + self.quarantine_secs))
            .collect();
        // Marked before the commit, so intake refuses them from before the
        // store holds them; settled against the store after it, whether it
        // held them, refused or failed (`2026-10-31-B-held-precommit.md`).
        // A failed settle holds them all: over-strict, never loose.
        let holding: Vec<[u8; 20]> = refilings
            .iter()
            .filter(|f| f.held.is_some())
            .flat_map(|f| f.hashes.iter().copied())
            .collect();
        let pending = self.held.pend(&holding);
        #[cfg(test)]
        if let Some(f) = tests::CORRECT_MARKED.get() {
            f(&store, &self.held, &holding);
        }
        let written = store.set_enrichments_at(generation, &[(id, r.enriched)], &refiled);
        let settled = pending.settle(|hs| store.held_among(hs));
        let written = written.map_err(internal)?;
        if written.is_none() {
            store.mark_unenriched(&[id]).map_err(internal)?;
            self.reenrich.store(true, Ordering::Release);
        }
        settled.map_err(internal)?;
        let held_by = refilings
            .into_iter()
            .find(|f| written.is_some() && f.held.is_some())
            .map(|f| f.rule);
        let c = r.class;
        Ok(CorrectResponse {
            adult_kind: c.adult_kind.map(|k| k.as_str().to_string()),
            held_by,
            junk: c.junk.score,
            content_type: c.content_type.as_str().to_string(),
        })
    }

    /// Your classification rules and how detection compares with your
    /// per-torrent corrections (§20.9): each correction is a fixture for
    /// adult detection, and the ones detection gets wrong are listed (up to
    /// 50) so a rule can cover their kind.
    pub fn classes(&self) -> ApiResult<Classes> {
        let rules = self.store().class_rules().map_err(internal)?;
        let (mut checked, mut disagree, mut seen) = (0, 0, 0);
        let mut wrong = Vec::new();
        let mut after = 0;
        while seen < MAX_CORRECTIONS {
            // A chunk read under the lock, classified outside it.
            let chunk = {
                let mut store = self.store();
                let page = store
                    .corrections_after(after, CORRECTIONS_CHUNK.min(MAX_CORRECTIONS - seen))
                    .map_err(internal)?;
                let mut rows = Vec::with_capacity(page.len());
                for (id, c) in &page {
                    let Some(adult) = c.adult else { continue };
                    if let (Some(row), Some(tree)) = (
                        store.torrent(*id).map_err(internal)?,
                        store.files(*id).map_err(internal)?,
                    ) {
                        rows.push((adult, row, tree));
                    }
                }
                (page.len(), page.last().map(|p| p.0), rows)
            };
            let (n, last, rows) = chunk;
            seen += n;
            for (adult, row, tree) in rows {
                let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
                let detected = lodestar_classify::classify(&row.name, &files).content_type
                    == ContentType::Adult;
                checked += 1;
                if detected != adult {
                    disagree += 1;
                    if wrong.len() < 50 {
                        wrong.push(DetectionExample {
                            adult,
                            hash: hex(&row.info_hash.0),
                        });
                    }
                }
            }
            match last {
                Some(id) if n == CORRECTIONS_CHUNK => after = id,
                _ => break,
            }
        }
        Ok(Classes {
            corrections: seen,
            detection: Detection {
                checked,
                disagree,
                examples: wrong,
            },
            rules: rules
                .into_iter()
                .map(|(id, query, adult)| ClassRule { adult, id, query })
                .collect(),
        })
    }

    /// Adds a classification rule and queues the stored torrents it
    /// matches for classifying again.
    pub fn add_class_rule(&self, body: &ClassRuleBody, now: i64) -> ApiResult<ClassRuleAdded> {
        let node = class_query(&body.query)?;
        let text = node.to_query();
        let id = {
            let mut store = self.store();
            if store.class_rules().map_err(internal)?.len() >= MAX_CLASS_RULES {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "too_many_class_rules",
                    format!(
                        "There are already {MAX_CLASS_RULES} classification rules: remove one, \
                         or widen one to cover more torrents."
                    ),
                ));
            }
            push_pending_requeue(&mut store, &text)?;
            let id = store.add_class_rule(&text, body.adult).map_err(internal)?;
            self.rules_changed(&mut store)?;
            id
        };
        let (queued, truncated) = self.requeue(&node, &text, now)?;
        Ok(ClassRuleAdded {
            id,
            queued,
            truncated,
        })
    }

    /// Removes a classification rule; the torrents it matched are
    /// classified again without it.
    pub fn remove_class_rule(&self, id: i64, now: i64) -> ApiResult<ClassRuleRemoved> {
        let mut store = self.store();
        let missing = || {
            not_found(
                "class_rule_not_found",
                "There's no such classification rule.",
            )
        };
        // Pending before the removal, as for an added rule.
        let rules = store.class_rules().map_err(internal)?;
        let q = rules
            .into_iter()
            .find(|r| r.0 == id)
            .map(|r| r.1)
            .ok_or_else(missing)?;
        let added = push_pending_requeue(&mut store, &q)?;
        let Some(q) = store.remove_class_rule(id).map_err(internal)? else {
            // Only its own entry: one already there is another change's.
            if added {
                drop_pending_requeue(&mut store, &q)?;
            }
            return Err(missing());
        };
        self.rules_changed(&mut store)?;
        drop(store);
        let (queued, truncated) = self.requeue_text(&q, now)?;
        Ok(ClassRuleRemoved { queued, truncated })
    }

    /// After a classification-rule change: the engine first, then a new
    /// generation (§20.9). Any enrichment that sees the new generation then
    /// also sees the new rules; anything made under the old ones is either
    /// refused at its write or covered by [`FiltersApi::requeue`].
    fn rules_changed(&self, store: &mut Store) -> ApiResult<()> {
        self.reload(store)?;
        store.bump_class_gen().map_err(internal)?;
        // A pass even without a search index: torrents a rejected batch
        // left marked are picked up.
        self.reenrich.store(true, Ordering::Release);
        Ok(())
    }

    /// Marks the stored torrents `node` matches for background enrichment,
    /// plus every torrent the index hasn't reached yet, and starts a pass.
    /// The search runs without the store lock, and marks go in chunks.
    /// `(None, false)` without a search index: nothing is marked now, and
    /// the entry stays for the first start with search on; `true` when the
    /// cap cut it short.
    ///
    /// `text` is the pending entry it clears once done: only when a search
    /// index did the marking, and no rule change came after the marking
    /// began (the entries are deduped, so a later change of the same query
    /// may share this one). Without an index it stays, and the first start
    /// with search on does it. Only the server's shutdown stops it
    /// (`server_stopping`); stopped or failed, the entry stays, and
    /// [`FiltersApi::resume_requeues`] redoes it at the next start. Marking
    /// again is harmless: it only queues a torrent for one more enrichment.
    fn requeue(&self, node: &Node, text: &str, now: i64) -> ApiResult<(Option<usize>, bool)> {
        // Every rule change up to this generation is in force before the
        // search starts, so these marks cover them all.
        let generation = self.store().class_gen().map_err(internal)?;
        let out = self.requeue_marks(node, now)?;
        if out.0.is_some() {
            let mut store = self.store();
            if store.class_gen().map_err(internal)? == generation {
                drop_pending_requeue(&mut store, text)?;
            }
        }
        Ok(out)
    }

    /// [`FiltersApi::requeue`] for a stored query text; one that no longer
    /// parses matches nothing, so its entry is just cleared.
    fn requeue_text(&self, text: &str, now: i64) -> ApiResult<(Option<usize>, bool)> {
        #[cfg(test)]
        if tests::FAIL_REQUEUE.with(|f| f.borrow().as_deref() == Some(text)) {
            return Err(internal("injected"));
        }
        match parse(text) {
            Ok(node) => self.requeue(&node, text, now),
            Err(_) => {
                drop_pending_requeue(&mut self.store(), text)?;
                Ok((None, false))
            }
        }
    }

    fn requeue_marks(&self, node: &Node, now: i64) -> ApiResult<(Option<usize>, bool)> {
        let Some(index) = self.index.as_ref().map(|h| h.index()) else {
            return Ok((None, false));
        };
        let stop = &self.shutdown;
        let check = || {
            if stop.is_stopped() {
                Err(stopping())
            } else {
                Ok(())
            }
        };
        check()?;
        // The watermark before the search: what's past it isn't searchable
        // yet, so it's all marked.
        let watermark = index.watermark().map_err(internal)?;
        index.refresh().map_err(internal)?;
        let compiled = index
            .query_cancelled(node, now, 0, stop)
            .map_err(|e| match e {
                lodestar_search::Error::Cancelled | lodestar_search::Error::OutOfTime => stopping(),
                e => internal(e),
            })?;
        check()?;
        let (ids, truncated) = index
            .matching_ids(&compiled, MAX_REQUEUE)
            .map_err(internal)?;
        let ids: Vec<i64> = ids.into_iter().map(|id| id as i64).collect();
        let mut n = 0;
        for chunk in ids.chunks(lodestar_store::MARK_CHUNK) {
            check()?;
            n += self.store().mark_unenriched(chunk).map_err(internal)?;
        }
        n += self
            .store()
            .mark_unenriched_after(watermark)
            .map_err(internal)?;
        self.reenrich.store(true, Ordering::Release);
        Ok((Some(n), truncated))
    }

    /// Redoes the pending re-queues: those a stop cut short, and those
    /// made while search was off (run at every start, in the background;
    /// search is turned on by a restart). Each one an index finishes is
    /// cleared; without an index they all stay. One that fails stays and
    /// the rest still run: its error is logged where it happens, by type
    /// only (`api.internal_error`), never with the query. Only the
    /// shutdown stops the loop, and what's left stays for the next start.
    /// Returns how many an index finished.
    pub fn resume_requeues(&self, now: i64) -> ApiResult<usize> {
        let pending = pending_requeues(&self.store())?;
        let mut done = 0;
        for text in &pending {
            match self.requeue_text(text, now) {
                Ok((Some(_), _)) => done += 1,
                Ok((None, _)) => {}
                Err(e) if self.shutdown.is_stopped() => return Err(e),
                Err(_) => {}
            }
        }
        Ok(done)
    }

    /// Tries a draft rule and the current rules on examples (the editor's
    /// live preview). Nothing is stored or logged.
    pub fn test(&self, body: &TestBody, now: i64) -> ApiResult<TestResponse> {
        if body.samples.len() > MAX_SAMPLES {
            return Err(bad(
                "too_many_samples",
                format!("Send at most {MAX_SAMPLES} samples."),
            )
            .field("samples"));
        }
        let draft = body.query.as_deref().map(query).transpose()?;
        let unless = body.unless.as_deref().map(query).transpose()?;
        let engine = self.engine.current();
        let mut results = Vec::with_capacity(body.samples.len());
        for s in &body.samples {
            let files: Vec<lodestar_classify::FileInfo> = s
                .files
                .iter()
                .take(lodestar_filter::MAX_FILES)
                .map(|f| {
                    let (path, size) = match f {
                        SampleFile::Path(p) => (p, 0),
                        SampleFile::Sized { path, size } => (path, *size),
                    };
                    lodestar_classify::FileInfo::new(
                        path.split('/').map(str::to_owned).collect(),
                        size,
                    )
                })
                .collect();
            let mut class = lodestar_classify::classify(&s.name, &files);
            if let Some(t) = s.content_type.as_deref() {
                class.content_type = ContentType::from_name(t).ok_or_else(|| {
                    bad("unknown_type", format!("There's no content type {t:?}.")).field("type")
                })?;
            }
            let facts = Facts {
                class: &class,
                size: files.iter().map(|f| f.size).sum(),
                piece_len: 0,
                flags: 0,
                discovered_at: now,
                now,
            };
            let item = Item {
                hashes: &[],
                name: &s.name,
                files: &files,
                content_type: class.content_type,
            };
            let doc = Doc::new(&item, Some(&facts));
            let matched = draft.as_ref().map(|q| {
                doc.matches(q, &[]) && !unless.as_ref().is_some_and(|u| doc.matches(u, &[]))
            });
            let v = engine.judge(&item, Some(&facts), now);
            results.push(TestResult {
                decision: v.decision.as_ref().map(|d| Decision {
                    action: d.action.as_str().to_string(),
                    rule: d.rule.id().to_string(),
                }),
                draft: matched,
                shadow: v.shadow.iter().map(|d| d.rule.id().to_string()).collect(),
            });
        }
        Ok(TestResponse { results })
    }
}

fn filters(s: &ApiState) -> ApiResult<Arc<FiltersApi>> {
    s.filters.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "filters_unavailable",
            "Filtering isn't available.",
        )
    })
}

/// The rule set, switches and lists.
#[utoipa::path(
    operation_id = "getFilters",get, path = "/filters", tag = "filters", responses((status = 200, body = Overview)))]
pub(crate) async fn overview(State(s): State<ApiState>) -> ApiResult<Json<Overview>> {
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.overview().map(Json))
        .await
        .map_err(internal)?
}

/// The review queue: quarantine (never with names) and the last 30 days'
/// decisions per rule.
#[utoipa::path(
    operation_id = "getFilterQueue",
    get,
    path = "/filters/queue",
    tag = "filters",
    params(QueueQuery),
    responses((status = 200, body = Queue))
)]
pub(crate) async fn queue(
    State(s): State<ApiState>,
    Query(page): Query<QueueQuery>,
) -> ApiResult<Json<Queue>> {
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.queue(crate::unix_now(), &page).map(Json))
        .await
        .map_err(internal)?
}

/// Tombstones (dropped torrents) by hash and rule, never with names, in
/// pages; `adult=true` lists only adult holds.
#[utoipa::path(
    operation_id = "getFilterTombstones",
    get,
    path = "/filters/tombstones",
    tag = "filters",
    params(TombstonesQuery),
    responses((status = 200, body = Tombstones))
)]
pub(crate) async fn tombstones(
    State(s): State<ApiState>,
    Query(page): Query<TombstonesQuery>,
) -> ApiResult<Json<Tombstones>> {
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.tombstones(&page).map(Json))
        .await
        .map_err(internal)?
}

/// The core rules' hashed examples, run against the rules as they are.
#[utoipa::path(
    operation_id = "getFilterFixtures",
    get,
    path = "/filters/fixtures",
    tag = "filters",
    responses((status = 200, body = Fixtures))
)]
pub(crate) async fn fixtures(State(s): State<ApiState>) -> ApiResult<Json<Fixtures>> {
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.fixtures().map(Json))
        .await
        .map_err(internal)?
}

/// Tries a draft rule and the current rules on example torrents. Nothing
/// is stored or logged.
#[utoipa::path(
    operation_id = "testFilterRule",
    post,
    path = "/filters/test",
    tag = "filters",
    request_body = TestBody,
    responses((status = 200, body = TestResponse))
)]
pub(crate) async fn test(
    State(s): State<ApiState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<TestBody>,
) -> ApiResult<Json<TestResponse>> {
    same_origin_json(&headers)?;
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.test(&body, crate::unix_now()).map(Json))
        .await
        .map_err(internal)?
}

/// How strict a candidate set of lists is (§2.9, §20.13): what it
/// would block and a warning matched to it. Nothing is stored or
/// connected.
#[utoipa::path(
    operation_id = "assessListStrictness",
    post,
    path = "/filters/strictness",
    tag = "filters",
    request_body = StrictnessBody,
    responses((status = 200, body = Strictness))
)]
pub(crate) async fn strictness(
    State(s): State<ApiState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<StrictnessBody>,
) -> ApiResult<Json<Strictness>> {
    same_origin_json(&headers)?;
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.strictness(&body).map(Json))
        .await
        .map_err(internal)?
}

/// Your classification correction for one stored torrent; it's
/// classified again at once.
#[utoipa::path(
    operation_id = "correctClassification",
    post,
    path = "/classify/correct",
    tag = "classify",
    request_body = CorrectBody,
    responses((status = 200, body = CorrectResponse))
)]
pub(crate) async fn correct(
    State(s): State<ApiState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<CorrectBody>,
) -> ApiResult<Json<CorrectResponse>> {
    same_origin_json(&headers)?;
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.correct(&body).map(Json))
        .await
        .map_err(internal)?
}

/// Your classification rules, and how detection compares with your
/// corrections.
#[utoipa::path(
    operation_id = "getClassify",get, path = "/classify", tag = "classify", responses((status = 200, body = Classes)))]
pub(crate) async fn classes(State(s): State<ApiState>) -> ApiResult<Json<Classes>> {
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.classes().map(Json))
        .await
        .map_err(internal)?
}

/// Adds a classification rule; the stored torrents it matches are
/// classified again. At most 1,000 rules: past that, 422
/// `too_many_class_rules`.
#[utoipa::path(
    operation_id = "addClassRule",
    post,
    path = "/classify/rules",
    tag = "classify",
    request_body = ClassRuleBody,
    responses((status = 200, body = ClassRuleAdded))
)]
pub(crate) async fn add_class_rule(
    State(s): State<ApiState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<ClassRuleBody>,
) -> ApiResult<Json<ClassRuleAdded>> {
    same_origin_json(&headers)?;
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.add_class_rule(&body, crate::unix_now()).map(Json))
        .await
        .map_err(internal)?
}

/// Removes a classification rule; the torrents it matched are classified
/// again without it.
#[utoipa::path(
    operation_id = "removeClassRule",
    post,
    path = "/classify/rules/remove",
    tag = "classify",
    request_body = IdBody,
    responses((status = 200, body = ClassRuleRemoved))
)]
pub(crate) async fn remove_class_rule(
    State(s): State<ApiState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<IdBody>,
) -> ApiResult<Json<ClassRuleRemoved>> {
    same_origin_json(&headers)?;
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.remove_class_rule(body.id, crate::unix_now()).map(Json))
        .await
        .map_err(internal)?
}

/// Starts a preview in the background; poll it by its id.
#[utoipa::path(
    operation_id = "startFilterPreviewJob",
    post,
    path = "/filters/preview/jobs",
    tag = "filters",
    request_body = ChangeBody,
    responses((status = 202, body = JobStarted))
)]
pub(crate) async fn start_preview(
    State(s): State<ApiState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<ChangeBody>,
) -> ApiResult<(StatusCode, Json<JobStarted>)> {
    same_origin_json(&headers)?;
    let f = filters(&s)?;
    let (id, job) = f.jobs.start()?;
    let now = crate::unix_now();
    tokio::task::spawn_blocking(move || {
        job.run(|progress| f.assess_tracked(&body.change, now, progress));
    });
    Ok((StatusCode::ACCEPTED, Json(JobStarted { job: id })))
}

/// A preview job: progress while it runs, then the assessment (or the
/// error) once it's done.
#[utoipa::path(
    operation_id = "getFilterPreviewJob",
    get,
    path = "/filters/preview/jobs/{id}",
    tag = "filters",
    params(("id" = u64, Path, description = "The job's id.")),
    responses((status = 200, body = JobStatus))
)]
pub(crate) async fn preview_job(
    State(s): State<ApiState>,
    Path(id): Path<u64>,
) -> ApiResult<Json<JobStatus>> {
    Ok(Json(filters(&s)?.jobs.get(id)?.status()))
}

/// Cancels a preview job; it stops at its next step.
#[utoipa::path(
    operation_id = "cancelFilterPreviewJob",
    post,
    path = "/filters/preview/jobs/{id}/cancel",
    tag = "filters",
    params(("id" = u64, Path, description = "The job's id.")),
    request_body(content = Object, description = "Any JSON, such as `{}`."),
    responses((status = 200, body = OkResponse))
)]
pub(crate) async fn cancel_preview(
    State(s): State<ApiState>,
    Path(id): Path<u64>,
    headers: HeaderMap,
) -> ApiResult<Json<OkResponse>> {
    same_origin_json(&headers)?;
    let job = filters(&s)?.jobs.get(id)?;
    job.progress.cancel();
    Ok(Json(OkResponse::OK))
}

/// What a change would do, without applying it or asking for the
/// password.
#[utoipa::path(
    operation_id = "previewFilterChange",
    post,
    path = "/filters/preview",
    tag = "filters",
    request_body = ChangeBody,
    responses((status = 200, body = Assessment))
)]
pub(crate) async fn preview(
    State(s): State<ApiState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<ChangeBody>,
) -> ApiResult<Json<Assessment>> {
    same_origin_json(&headers)?;
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.assess(&body.change, crate::unix_now()).map(Json))
        .await
        .map_err(internal)?
}

/// Applies a change. A change that weakens filtering is refused with 428
/// (`confirm_required`, the assessment beside the error) until it's sent
/// again with the admin password, every time (§20.11).
#[utoipa::path(
    operation_id = "applyFilterChange",
    post,
    path = "/filters/apply",
    tag = "filters",
    request_body = ChangeBody,
    responses((status = 200, body = ApplyResponse))
)]
pub(crate) async fn apply(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    JsonBody(body): JsonBody<ChangeBody>,
) -> ApiResult<Json<ApplyResponse>> {
    same_origin_json(&headers)?;
    let f = filters(&s)?;
    let auth = s.auth.clone();
    tokio::task::spawn_blocking(move || apply_change(&f, &auth, ip, body))
        .await
        .map_err(internal)?
}

fn apply_change(
    f: &FiltersApi,
    auth: &auth::Auth,
    ip: IpAddr,
    body: ChangeBody,
) -> ApiResult<Json<ApplyResponse>> {
    let now = crate::unix_now();
    let assessment = f.assess(&body.change, now)?;
    if assessment.confirm {
        match body.password.as_deref() {
            None | Some("") => {
                // The assessment rides beside the error: the warning the
                // client shows before asking for the password.
                return Err(ApiError::new(
                    StatusCode::PRECONDITION_REQUIRED,
                    "confirm_required",
                    "This change weakens filtering: read the warning and confirm with your password.",
                )
                .field("password")
                .assessment(assessment));
            }
            Some(p) => {
                // The password confirms the entries the warning listed, so
                // a removal names them: what's pending can have grown since.
                if let Change::ResolveFloorRemoval {
                    hashes: None,
                    remove: true,
                    ..
                } = &body.change
                {
                    return Err(bad(
                        "floor_removal_needs_hashes",
                        "Name the entries to remove: send back the floor_entries the warning listed as hashes.",
                    )
                    .field("hashes"));
                }
                auth.confirm(ip, p).map_err(|e| auth_error(e, "password"))?
            }
        }
    }
    f.apply(&body.change, assessment, now).map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_filter::{Core, CsList, Lists, Switches};

    /// Runs in `correct` right after it marks its hashes pending, before
    /// its store write (`2026-10-31-B-held-precommit.md`).
    pub(super) type CorrectHook = fn(&Store, &HeldSet, &[[u8; 20]]);

    thread_local! {
        pub(super) static CORRECT_MARKED: std::cell::Cell<Option<CorrectHook>> =
            const { std::cell::Cell::new(None) };
        /// A pending re-queue text that fails every time it's redone.
        pub(super) static FAIL_REQUEUE: std::cell::RefCell<Option<String>> =
            const { std::cell::RefCell::new(None) };
    }

    /// One pending re-queue failing every time doesn't hold up the rest:
    /// it stays, and the others are done and cleared.
    #[test]
    fn a_failing_requeue_does_not_block_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("t.db"), Default::default()).unwrap();
        for q in ["fails", "lexb", "lexb", "other"] {
            push_pending_requeue(&mut store, q).unwrap();
        }
        assert_eq!(
            pending_requeues(&store).unwrap(),
            ["fails", "lexb", "other"]
        );
        let api = FiltersApi {
            store: Arc::new(Mutex::new(store)),
            engine: Arc::new(Handle::new(Engine::new(Core::new(
                Lists::shipped().unwrap(),
                CsList::default(),
                Switches::default(),
            )))),
            held: Default::default(),
            sealer: None,
            ips: Default::default(),
            index: Some(lodestar_search::SearchIndex::in_memory().unwrap().into()),
            reenrich: Default::default(),
            jobs: Default::default(),
            quarantine_secs: 0,
            shutdown: Default::default(),
        };
        FAIL_REQUEUE.with(|f| *f.borrow_mut() = Some("fails".into()));
        assert_eq!(api.resume_requeues(0).unwrap(), 2);
        assert_eq!(pending_requeues(&api.store()).unwrap(), ["fails"]);
        // Only the shutdown stops the loop.
        api.shutdown.cancel();
        push_pending_requeue(&mut api.store(), "lexb").unwrap();
        assert!(api.resume_requeues(0).is_err());
        assert_eq!(pending_requeues(&api.store()).unwrap(), ["fails", "lexb"]);
    }

    /// `correct` marks a new hold before its store write: at the mark the
    /// store doesn't hold it yet, and a release's M landing there (reading
    /// the store as not holding it) can't make it fetchable. After the
    /// write's reconcile it's held, as the store has it, with no mark left.
    #[test]
    fn correct_marks_a_new_hold_before_its_write() {
        use lodestar_bencode::{OwnedValue, to_vec};
        use lodestar_filter::TermSet;
        use std::sync::atomic::AtomicUsize;
        static HOOKS: AtomicUsize = AtomicUsize::new(0);
        fn marked(store: &Store, held: &HeldSet, hs: &[[u8; 20]]) {
            HOOKS.fetch_add(1, Ordering::Relaxed);
            assert!(!hs.is_empty() && held.pending_len() > 0);
            assert!(
                store.held_among(hs).unwrap().iter().all(|h| !h),
                "marked before the write"
            );
            held.remove_released(hs, |hs| store.held_among(hs)).unwrap();
            assert!(hs.iter().all(|h| held.contains(h)), "pending, so refused");
        }
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("t.db"), Default::default()).unwrap();
        let name = "Holiday Rolea one two three four five six seven eight nine Lexa.mp4";
        let raw = to_vec(&OwnedValue::Dict(
            [
                (b"length".to_vec(), OwnedValue::Int(900 << 20)),
                (
                    b"name".to_vec(),
                    OwnedValue::Bytes(name.as_bytes().to_vec()),
                ),
                (b"piece length".to_vec(), OwnedValue::Int(1 << 20)),
                (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
            ]
            .into_iter()
            .collect(),
        ));
        let info = lodestar_metainfo::Info::parse(&raw).unwrap();
        store
            .insert(&[lodestar_store::NewTorrent {
                info: &info,
                raw_info: Some(&raw),
                discovered_at: 1,
                metadata_at: 1,
                extra_flags: 0,
                source: lodestar_store::SourceId::new(0).unwrap(),
            }])
            .unwrap();
        let hash = info.primary_hash().0;
        let api = FiltersApi {
            store: Arc::new(Mutex::new(store)),
            // Stand-in terms: only the Adult-only CS-5a catches them.
            engine: Arc::new(Handle::new(Engine::new(Core::new(
                Lists {
                    sexual: TermSet::of(&["lexa"]),
                    child_roles: TermSet::of(&["rolea"]),
                    codes: TermSet::of(&["codex"]),
                },
                CsList::default(),
                Switches::default(),
            )))),
            held: Default::default(),
            sealer: None,
            ips: Default::default(),
            index: None,
            reenrich: Default::default(),
            jobs: Default::default(),
            quarantine_secs: 86_400,
            shutdown: Default::default(),
        };
        api.held.insert(hash);
        CORRECT_MARKED.set(Some(marked));
        let r = api.correct(&CorrectBody {
            hash: hash.iter().map(|b| format!("{b:02x}")).collect(),
            adult: Some(true),
            not_junk: false,
        });
        CORRECT_MARKED.set(None);
        assert_eq!(r.unwrap().held_by.as_deref(), Some("CS-5a"));
        assert_eq!(HOOKS.load(Ordering::Relaxed), 1, "the hook ran");
        assert!(api.held.contains(&hash));
        assert_eq!(api.held.pending_len(), 0);
        let stored: HashSet<_> = api
            .store()
            .filter_state()
            .unwrap()
            .held
            .into_iter()
            .map(|(h, _)| h)
            .collect();
        assert_eq!(api.held.snapshot(), stored);
    }

    fn done() -> ApiResult<Assessment> {
        Ok(Assessment::default())
    }

    #[test]
    fn a_cancelled_preview_reports_cancelled() {
        let jobs = PreviewJobs::default();
        let (_, job) = jobs.start().unwrap();
        assert_eq!(job.status().state, JobState::Running);
        job.progress.cancel();
        job.run(|p| p.step(1).map(|()| Assessment::default()));
        let st = job.status();
        assert_eq!(st.state, JobState::Cancelled);
        assert_eq!((st.error, st.status), (None, None));
        let (_, other) = jobs.start().unwrap();
        other.run(|_| Err(conflict("floor_locked", "x")));
        assert_eq!(other.status().state, JobState::Failed);
        assert_eq!(other.status().status, Some(409));
    }

    #[test]
    fn at_most_four_previews_run_at_once() {
        let jobs = PreviewJobs::default();
        let started: Vec<_> = (0..MAX_JOBS).map(|_| jobs.start().unwrap()).collect();
        let err = jobs.start().unwrap_err();
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
        // One finishing frees a slot.
        started[0].1.run(|_| done());
        assert!(jobs.start().is_ok());
    }

    #[test]
    fn a_panicking_preview_finishes_with_a_500_and_frees_its_slot() {
        let jobs = PreviewJobs::default();
        let (id, job) = jobs.start().unwrap();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            job.run(|_| panic!("boom"));
        }));
        assert!(caught.is_err());
        let job = jobs.get(id).unwrap();
        assert!(job.finished_at().is_some());
        let result = job.result.lock().unwrap();
        let err = result.as_ref().unwrap().as_ref().unwrap_err();
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
        drop(result);
        for _ in 0..MAX_JOBS {
            jobs.start().unwrap();
        }
    }

    #[test]
    fn finished_previews_expire_and_are_capped() {
        let jobs = PreviewJobs::default();
        let (id, job) = jobs.start().unwrap();
        job.run(|_| done());
        assert!(jobs.get(id).is_ok());
        // Ten minutes on, it's gone on read.
        let old = Instant::now()
            .checked_sub(JOB_KEEP + Duration::from_secs(1))
            .unwrap();
        *job.finished.lock().unwrap() = Some(old);
        assert_eq!(jobs.get(id).unwrap_err().status(), StatusCode::NOT_FOUND);

        // At most 64 kept; the oldest finished go first, running ones stay.
        let (running, _) = jobs.start().unwrap();
        let mut first = None;
        for _ in 0..(MAX_RETAINED + 10) {
            let (id, job) = jobs.start().unwrap();
            job.run(|_| done());
            first.get_or_insert(id);
        }
        assert!(jobs.jobs().len() <= MAX_RETAINED);
        assert!(jobs.get(running).is_ok(), "a running job is never evicted");
        assert!(
            jobs.get(first.unwrap()).is_err(),
            "the oldest finished went"
        );
    }
}
