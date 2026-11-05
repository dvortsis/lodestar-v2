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
use lodestar_filter::load::{
    KIND_HASHES, KIND_KEYWORD, KIND_PAYLOAD, KIND_QUERY, STAND_INS_KEY, StandIns, decode_match,
    encode_match, stored_rule_reads,
};
use lodestar_filter::packs::manifest::{self as pack_manifest, Manifest};
use lodestar_filter::packs::overrides::{
    self as pack_overrides, Effect as OverrideEffect, PackOverride, PackOverrides,
};
use lodestar_filter::packs::upgrade::{self as pack_upgrade, Applied};
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
    Overview, PackOverrideState, PackRuleState, PackState, PendingFloorEntry, QuarantineEntry,
    Queue, QueueQuery, RuleEntry, Strictness, TestResponse, TestResult, TombstoneEntry, Tombstones,
    TombstonesQuery,
};
pub use crate::dto::filters::{
    Assessment, Change, ChangeBody, ClassRuleBody, CorrectBody, IdBody, MatchInput, Preview,
    Sample, SampleFile, StrictnessBody, TestBody,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{ClientIp, Json as JsonBody, Path, Query, same_origin_json};

mod packs;
mod standin;
pub use standin::{Rejudged, stand_in_notice};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(
    overview,
    queue,
    tombstones,
    fixtures,
    test,
    strictness,
    subscriptions::list,
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

mod subscriptions;

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
    /// The registered always-adult sources, as a bitset over source ids
    /// (X-source-adult; `lodestar_pipeline::Pipeline::adult_sources`). A
    /// correction never un-adults a torrent one of them found unless it
    /// says Not adult. Empty while none is registered.
    pub adult_sources: u64,
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
            .route("/filters/subscriptions", get(subscriptions::list))
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

fn nothing_held() -> ApiError {
    conflict("nothing_held", "No version of that list is held.")
}

fn internal_none() -> ApiError {
    internal("a subscribe change without a list")
}

/// The state changed since the preview so that this change now weakens
/// filtering (or weakens it differently): it must be previewed and
/// confirmed again.
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
    let node = parse(text).map_err(|e| {
        bad(
            "query_syntax",
            format!("The query doesn't parse: {}", e.message),
        )
        .span(e.start, e.end)
    })?;
    // The judge doesn't know an item's provenance: claims and fetched
    // metadata are judged alike, and its sources aren't known yet
    // (SRC-3). A rule on them would silently never match, or with `-`,
    // match everything.
    if node.mentions_provenance() {
        return Err(bad(
            "provenance_in_rule",
            "Rules can't test verified:, source: or network:. Filters judge what a torrent \
             says it is, before search knows where it came from.",
        ));
    }
    Ok(node)
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

/// The exceptions by hash to SP-2 and SP-3 in `set`, one per hash: those
/// whose originals' SP-4 signatures they release.
fn sp4_hash_exceptions(set: &RuleSet) -> HashSet<(PackRule, [u8; 20])> {
    set.exceptions
        .iter()
        .filter_map(|e| {
            let rule = PackRule::from_id(&e.rule)
                .filter(|r| matches!(r, PackRule::Sp2 | PackRule::Sp3))?;
            match decode_match(e.kind, &e.value)? {
                Match::Hashes(hs) => Some(hs.into_iter().map(move |h| (rule, h))),
                _ => None,
            }
        })
        .flatten()
        .collect()
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

/// One pack rule by id (`MW-2`).
fn pack_rule(id: &str) -> ApiResult<PackRule> {
    PackRule::from_id(id).ok_or_else(|| {
        bad("unknown_pack_rule", format!("There's no pack rule {id:?}.")).field("rule")
    })
}

/// An override's action: one of [`pack_overrides::ACTIONS`].
fn override_action(name: &str) -> ApiResult<Action> {
    pack_overrides::action_named(name)
        .filter(|a| pack_overrides::ACTIONS.contains(a))
        .ok_or_else(|| {
            bad(
                "unknown_action",
                "A pack rule's action can be flag, demote, hide or drop.",
            )
            .field("action")
        })
}

/// What a pack-rule override change does to the rule as stored now (RP-2).
#[derive(Debug, Clone, PartialEq, Eq)]
struct OverrideChange {
    rule: PackRule,
    /// The override to write; `None` removes it.
    set: Option<PackOverride>,
    /// It lets more through, or holds more lightly, than what applies now.
    weakening: bool,
}

/// Reads a pack-rule override change against the store, as the next load
/// reads it: the engine can lag the store after an apply that failed
/// part-way. In the preview and again under the lock in apply, so a
/// change previewed as strengthening that weakens by then is refused
/// (§20.7's `changed_since_preview`).
///
/// Setting weakens when it has an `unless`, or its action is lighter than
/// what applies now (the rule's own, or a heavier override's). Removing
/// weakens when the override applies an action heavier than the rule's
/// own. A weakening override can't be set while the version this install
/// applied is in doubt: it would be bound to nothing it could be checked
/// against.
fn override_change(
    store: &Store,
    manifest: &Manifest,
    change: &Change,
) -> ApiResult<OverrideChange> {
    let set = store.rule_set().map_err(internal)?;
    let versions = pack_upgrade::PackVersions::resolve(manifest, &pack_upgrade::Rows::of(&set));
    let stored = PackOverrides::resolve(manifest, &versions, &set.pack_overrides);
    let top = |rule: PackRule| packs::applied_action(manifest, &versions, &stored, rule);
    match change {
        Change::SetPackOverride {
            rule,
            action: act,
            unless,
            ..
        } => {
            let r = pack_rule(rule)?;
            let action = override_action(act)?;
            let unless = unless
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(|u| query(u).map_err(|e| e.field("unless")))
                .transpose()?;
            // An `unless` that matches every torrent switches the rule off:
            // that's the pack switch's change, with its own warning.
            if unless.as_ref().is_some_and(|n| *n == Node::All) {
                return Err(bad(
                    "unless_matches_everything",
                    "This unless matches every torrent, which turns the rule off: switch the rule off instead.",
                )
                .field("unless"));
            }
            let unless = unless.map(|n| n.to_query());
            let weakening = pack_overrides::weakens(action, unless.is_some(), top(r));
            let base = pack_overrides::current_base(&versions, r);
            if weakening && base == pack_upgrade::UNKNOWN {
                return Err(conflict(
                    "pack_rule_in_doubt",
                    format!(
                        "The version of {rule} this install applied isn't known for sure, so an \
                         override that weakens it couldn't be checked against it. Set it again \
                         once the Filters & Safety notice about pack versions is resolved."
                    ),
                ));
            }
            Ok(OverrideChange {
                rule: r,
                set: Some(PackOverride {
                    base,
                    action,
                    unless,
                }),
                weakening,
            })
        }
        Change::RemovePackOverride { rule } => {
            let r = pack_rule(rule)?;
            if !set.pack_overrides.iter().any(|(id, _)| id == r.id()) {
                return Err(not_found("entry_not_found", "There's no such entry."));
            }
            let base = pack_overrides::base_action(manifest, &versions, r);
            let weakening = match stored.get(r).map(|l| &l.effect) {
                Some(OverrideEffect::Replace(a, _) | OverrideEffect::AtLeast(a)) => *a > base,
                Some(OverrideEffect::None) | None => false,
            };
            Ok(OverrideChange {
                rule: r,
                set: None,
                weakening,
            })
        }
        _ => Err(internal("not a pack-rule override change")),
    }
}

/// A `confirm_pack_override` as the `set_pack_override` it amounts to:
/// the stored override's action and `unless`, bound to the version that
/// applies now. Only an override whose base changed can be confirmed, and
/// only one that reads (one that doesn't is set again in full).
fn confirm_as_set(store: &Store, manifest: &Manifest, change: &Change) -> ApiResult<Change> {
    let Change::ConfirmPackOverride { rule, base } = change else {
        return Err(internal("not a confirm_pack_override"));
    };
    let r = pack_rule(rule)?;
    let set = store.rule_set().map_err(internal)?;
    let versions = pack_upgrade::PackVersions::resolve(manifest, &pack_upgrade::Rows::of(&set));
    let stored = PackOverrides::resolve(manifest, &versions, &set.pack_overrides);
    let o = stored
        .get(r)
        .ok_or_else(|| not_found("entry_not_found", "There's no such entry."))?;
    let (Some(action), false) = (o.decoded.action, o.unreadable()) else {
        return Err(conflict(
            "pack_override_unreadable",
            format!("Your override of {rule} can't be read: set it again in full, or remove it."),
        ));
    };
    if !o.changed {
        return Err(conflict(
            "pack_override_current",
            format!("Your override of {rule} is already bound to the version that applies."),
        ));
    }
    Ok(Change::SetPackOverride {
        rule: rule.clone(),
        action: action.as_str().to_owned(),
        unless: o.unless_text().map(str::to_owned),
        base: *base,
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
            "Switching off {id} stops it hiding or dropping malware, lures, fakes, spam, scams \
             or unauthorised copies it would have caught. {n} Safer: an exception for the \
             torrents it got wrong."
        ),
        Change::SetPackOverride { rule, unless, .. } => {
            let what = if unless.as_deref().is_some_and(|u| !u.is_empty()) {
                format!(
                    "Torrents your \"unless\" matches skip {rule} altogether, and the rest get the \
                     action you chose"
                )
            } else {
                format!("Torrents {rule} catches get a lighter action than it applies now")
            };
            format!(
                "{what}, so it may stop hiding or dropping malware, lures, fakes, spam or scams it \
                 would have caught. Other rules still check them. If a newer version of {rule} is \
                 accepted, this override pauses until you confirm it again. {n} Safer: an \
                 exception for the exact torrents it got wrong."
            )
        }
        Change::RemovePackOverride { rule } => {
            format!("Removing your override puts {rule} back to its own, lighter action. {n}")
        }
        Change::ConfirmPackOverride { rule, .. } => format!(
            "{rule} changed since you set your override, and your override would make the rule \
             as it is now hold less, or more lightly: confirming it again applies it to the new \
             version, so it may stop hiding or dropping malware, lures, fakes, spam or scams the \
             new version would catch. Other rules still check them. {n} Safer: keep it paused, \
             or set a new override for the new version."
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
             fakes, spam, scams, unauthorised copies). The child-safety core still applies. {n} Safer: narrow it, \
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
            Change::SetPackOverride { .. }
            | Change::RemovePackOverride { .. }
            | Change::ConfirmPackOverride { .. } => {
                let store = self.store();
                let set;
                let change = if let Change::ConfirmPackOverride { .. } = change {
                    set = confirm_as_set(&store, &engine.manifest, change)?;
                    &set
                } else {
                    change
                };
                let o = override_change(&store, &engine.manifest, change)?;
                a.pack_base = o.set.as_ref().map(|s| s.base);
                if o.weakening {
                    a.weakening = true;
                    a.preview = rules(vec![o.rule.id().to_owned()]);
                }
            }
            Change::AcceptPackVersions {
                rules: ids,
                versions,
            } => {
                if versions
                    .as_ref()
                    .is_some_and(|v| v.len() > packs::MAX_NAMED)
                {
                    return Err(bad("too_many_versions", "That's more versions than exist.")
                        .field("versions"));
                }
                let acc = packs::acceptance(&self.store(), &engine.manifest, ids, now)?;
                // Nothing is released: the counts are what the accepted
                // versions hold in the replay window, from their shadow.
                a.preview = Preview {
                    caught_30d: acc.caught,
                    per_day: acc.caught as f64 / 30.0,
                    ..Preview::default()
                };
                a.weakening = acc.weakening;
                if acc.weakening {
                    a.warning = Some(packs::warning(&acc));
                }
                a.pack_versions = Some(acc.versions);
                a.pack_acceptance = Some(acc.impact);
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
            Change::Unsubscribe { id } | Change::PauseSubscription { id, enabled: false } => {
                let store = self.store();
                let sub = subscriptions::subscription(&store, *id)?;
                a.weakening = true;
                a.preview = rules(vec![subscriptions::rule_id(*id)]);
                a.preview.removed_from_floor = subscriptions::floor_leaving(&store, &engine, &sub)?;
            }
            Change::PauseSubscription { id, .. } => {
                subscriptions::subscription(&self.store(), *id)?;
            }
            Change::ResolveHeld { id, accept, .. } => {
                let store = self.store();
                let sub = subscriptions::subscription(&store, *id)?;
                let effect = subscriptions::held_effect(&store, &sub)?.ok_or_else(nothing_held)?;
                a.held_version = Some(effect.version.clone());
                // Any entry the held version drops stops being blocked,
                // whether it shrank or grew. A floor-marked list's removals
                // wait for a decision of their own (§2.7), so accepting one
                // releases nothing.
                if *accept && effect.loosens && !sub.floor {
                    let (quarantine, tombstones, _) =
                        subscriptions::dropped_holds(&store, *id, &effect.dropped)?;
                    a.weakening = true;
                    a.unblocked = Some(effect.unblocked);
                    // Exact: the list's holds on what it drops. The 30-day
                    // figures count the whole list, so they're left out.
                    a.preview = Preview {
                        quarantine,
                        tombstones,
                        ..Preview::default()
                    };
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
            Change::Subscribe { .. } => {
                // Validated here too, so the preview reports what apply
                // would refuse.
                let new = subscriptions::check_subscribe(change)?;
                a.confirm = new.is_some_and(|n| n.floor);
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
            if a.warning.is_none() {
                a.warning = Some(warning(
                    change,
                    &a.preview,
                    a.neutralize.as_ref(),
                    adult_hold,
                ));
            }
        }
        if let (
            true,
            Change::Unsubscribe { id }
            | Change::PauseSubscription { id, .. }
            | Change::ResolveHeld { id, .. },
        ) = (a.weakening, change)
        {
            let sub = subscriptions::subscription(&self.store(), *id)?;
            let n = count_line(&a.preview);
            let unblocked = a.unblocked.unwrap_or_default();
            a.warning = Some(subscriptions::warning(
                change, &sub, &a.preview, unblocked, &n,
            ));
        }
        a.strictness = subscriptions::strictness_after(&self.store(), change)?;
        progress.begin(DONE, 0);
        Ok(a)
    }

    /// Seeds the pack versions if the store never was (RP-0). A seeding
    /// on a store that already had rules is recorded in the marker
    /// (`PackVersions::seeded_over_rules`): an upgrade from before RP-0, or
    /// an install whose version rows were all lost. The pipeline's start
    /// seeds first, so this is the fallback. Returns whether it seeded.
    fn seed_packs(&self, store: &mut Store, now: i64) -> ApiResult<bool> {
        let manifest = self.engine.current().manifest.clone();
        Ok(pack_upgrade::ensure_seeded(store, &manifest, now)
            .map_err(internal)?
            .is_some())
    }

    /// Records the pack-version conditions in `filter.stand_ins`
    /// (`packs.*` entries), replacing the ones recorded before: the
    /// Filters & Safety notice reads them (RP-0). Entries of other kinds
    /// are left as they are.
    fn note_pack_conditions(&self, store: &mut Store, now: i64) -> ApiResult<()> {
        // From the store's rows, as the next load reads them: the engine
        // can lag the store after an apply that failed part-way.
        let set = store.rule_set().map_err(internal)?;
        let manifest = self.engine.current().manifest.clone();
        let versions =
            pack_upgrade::PackVersions::resolve(&manifest, &pack_upgrade::Rows::of(&set));
        let mut labels = versions.labels();
        // Overrides whose rows don't read (RP-2): they fail closed, and are
        // listed until they read again or are removed.
        labels.extend(
            PackOverrides::resolve(&manifest, &versions, &set.pack_overrides)
                .iter()
                .filter(|o| o.unreadable())
                .map(|o| pack_overrides::label(o.rule)),
        );
        // SP-4's signature rows that didn't read at start (RP-5).
        labels.extend(self.engine.current().spam_sig_labels());
        let text = store.setting_text(STAND_INS_KEY).map_err(internal)?;
        let before = text
            .as_deref()
            .and_then(StandIns::decode)
            .unwrap_or_default();
        packs::note_shadows(store, &manifest)?;
        let after = before.with_packs(&labels, now);
        if after == before {
            return Ok(());
        }
        if after.0.is_empty() {
            store.update_settings_text(&[], &[STAND_INS_KEY])
        } else {
            store.set_settings_text(&[(STAND_INS_KEY, &after.encode())])
        }
        .map_err(internal)
    }

    /// At start: seeding if needed, the engine reloaded when it seeded,
    /// and the conditions recorded.
    fn start_packs(&self, now: i64) -> ApiResult<()> {
        let mut store = self.store();
        if self.seed_packs(&mut store, now)? {
            self.reload(&store)?;
        }
        self.note_pack_conditions(&mut store, now)?;
        let _ = packs::prune_review(&mut store);
        Ok(())
    }

    /// Accepts the pending versions of `rules` (RP-3 puts it behind
    /// `POST /filters/preview` and `apply` with the weakening assessment):
    /// the applied versions switch, the engine reloads, so the writer's
    /// sweeps re-judge through the ordinary rule-change path, and nothing
    /// is released (plan Q7). Not part of what Undo restores. Returns the
    /// record written.
    pub fn accept_pack_versions(&self, rules: &[PackRule], now: i64) -> ApiResult<Applied> {
        let mut guard = self.store();
        let store = &mut *guard;
        self.seed_packs(store, now)?;
        let manifest = self.engine.current().manifest.clone();
        let applied = pack_upgrade::accept(store, &manifest, rules, now).map_err(internal)?;
        self.reload(store)?;
        self.note_pack_conditions(store, now)?;
        Ok(applied)
    }

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
            // As stored, readable or not: an unreadable row stays one.
            "pack_overrides": set.pack_overrides.iter().map(|(rule, v)| match v {
                Ok(t) => json!({"rule": rule, "text": t}),
                Err(b) => json!({"rule": rule, "hex": hex(b)}),
            }).collect::<Vec<_>>(),
            "core": switches,
            // Taken after RP-0's seeding: SC-2 has its own rows in it.
            "packs_seeded": true,
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
        let mut set = RuleSet {
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
            // A snapshot from before RP-2 has none, as that set had none.
            pack_overrides: arr("pack_overrides")
                .iter()
                .filter_map(|o| {
                    let rule = o["rule"].as_str()?.to_owned();
                    let value = match o["text"].as_str() {
                        Some(t) => Ok(t.to_owned()),
                        None => Err(unhex(&o["hex"])),
                    };
                    Some((rule, value))
                })
                .collect(),
            // Never restored: Undo leaves the applied pack versions.
            ..RuleSet::default()
        };
        if v.get("packs_seeded").is_none() {
            // Taken before RP-0 split SC-2 out of SC-1: SC-2 gets SC-1's
            // switch and exceptions, as seeding gave it.
            set.pack_switches = pack_upgrade::copy_sc1_switch(&set.pack_switches);
            let mut next = set.exceptions.iter().map(|e| e.id).max().unwrap_or(0);
            let copies: Vec<ExceptionRow> = set
                .exceptions
                .iter()
                .filter(|e| e.rule == PackRule::Sc1.id())
                .map(|e| {
                    next += 1;
                    ExceptionRow {
                        id: next,
                        rule: PackRule::Sc2.id().to_owned(),
                        ..e.clone()
                    }
                })
                .collect();
            set.exceptions.extend(copies);
        }
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

    /// Releases `hash`: its quarantine or tombstone, and, as the release
    /// hook (SP-4, RP-5), its payload signature if it's a stored torrent
    /// a spam rule hid, so its clones follow it. Returns whether there was
    /// anything to release.
    fn release(&self, store: &mut Store, hash: [u8; 20]) -> ApiResult<bool> {
        let had = store.release(&hash).map_err(internal)?;
        self.unhold(store, &[hash])?;
        let followed = self.release_clones(store, &[hash], None)?;
        Ok(had || followed > 0)
    }

    /// SP-4 (RP-5) after a change that removed or restored exceptions
    /// (removing one, Undo, redo): an exception by hash to SP-2 or SP-3
    /// that's gone puts back the signature of the stored original it
    /// released, if that original is still hidden (or, for SP-3, demoted),
    /// so its clones are held again; one that's back removes it again.
    /// The action comes from the original's stored mark. One by payload
    /// that's gone can't be put back: the original isn't known.
    fn follow_exceptions(
        &self,
        store: &mut Store,
        before: &HashSet<(PackRule, [u8; 20])>,
    ) -> ApiResult<()> {
        let after = sp4_hash_exceptions(&store.rule_set().map_err(internal)?);
        for (rule, hash) in after.difference(before) {
            self.release_clones(store, &[*hash], Some(*rule))?;
        }
        let engine = self.engine.current();
        let mut restored = false;
        for (rule, hash) in before.difference(&after) {
            let Some(id) = store
                .find(&lodestar_metainfo::InfoHash(*hash))
                .map_err(internal)?
            else {
                continue;
            };
            let Some(row) = store.torrent(id).map_err(internal)? else {
                continue;
            };
            let action = if row.flags & lodestar_store::flags::HIDDEN != 0 {
                lodestar_filter::Action::Hide
            } else if row.flags & lodestar_store::flags::DEMOTED != 0 {
                lodestar_filter::Action::Demote
            } else {
                continue;
            };
            let Some(original) = lodestar_filter::spam_sigs::Original::of(*rule, action) else {
                continue;
            };
            let Some(tree) = store.files(id).map_err(internal)? else {
                continue;
            };
            let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
            if let Some(sig) =
                lodestar_filter::spam_sigs::usable(lodestar_filter::payload_sig(&files))
            {
                engine.spam_sigs.add(sig, original);
                restored = true;
            }
        }
        if restored {
            engine.spam_sigs.flush(store).map_err(internal)?;
        }
        Ok(())
    }

    /// SP-4's release hook (RP-5): removes from the signature set the
    /// payload signatures of the stored torrents among `hashes`, so the
    /// clones of an original that's released or excepted follow it. With
    /// `rule`, only a signature that rule added (an exception to SP-2
    /// leaves what SP-3 hid). A hash that isn't stored, or whose payload
    /// isn't specific, has none. Returns how many were in the set.
    fn release_clones(
        &self,
        store: &mut Store,
        hashes: &[[u8; 20]],
        rule: Option<PackRule>,
    ) -> ApiResult<usize> {
        let engine = self.engine.current();
        if engine.spam_sigs.is_empty() {
            return Ok(0);
        }
        let mut sigs = Vec::new();
        for h in hashes {
            let Some(id) = store
                .find(&lodestar_metainfo::InfoHash(*h))
                .map_err(internal)?
            else {
                continue;
            };
            if let Some(tree) = store.files(id).map_err(internal)? {
                let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
                sigs.extend(lodestar_filter::spam_sigs::usable(
                    lodestar_filter::payload_sig(&files),
                ));
            }
        }
        if sigs.is_empty() {
            return Ok(0);
        }
        engine
            .spam_sigs
            .release(store, &sigs, rule)
            .map_err(internal)
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
        self.unhold(store, hashes)?;
        self.release_clones(store, hashes, None).map(|_| ())
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
        // Pack versions are seeded before anything writes a switch or an
        // exception, so the seeding's SC-1 → SC-2 copy sees the set as it
        // was (RP-0).
        self.seed_packs(store, now)?;
        let rule_layers = matches!(
            change,
            Change::CoreSwitch { .. }
                | Change::PackSwitch { .. }
                | Change::AddRule { .. }
                | Change::UpdateRule { .. }
                | Change::RemoveRule { .. }
                | Change::SetPackOverride { .. }
                | Change::RemovePackOverride { .. }
                | Change::ConfirmPackOverride { .. }
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
        // SP-4 (RP-5): the exceptions by hash to SP-2 and SP-3 before a
        // change that can remove or restore them, so their originals'
        // signatures follow (`follow_exceptions`).
        let sp4_before = if matches!(change, Change::RemoveException { .. } | Change::Undo) {
            Some(sp4_hash_exceptions(&store.rule_set().map_err(internal)?))
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
                // Off → on records the versions it was previewed against
                // (RP-0), in the same transaction as the switch.
                let manifest = self.engine.current().manifest.clone();
                pack_upgrade::set_pack_switch(store, &manifest, id, *on, now).map_err(internal)?;
                if !on {
                    out.released = Some(self.release_rules(store, &rules)?);
                }
            }
            Change::SetPackOverride { .. }
            | Change::RemovePackOverride { .. }
            | Change::ConfirmPackOverride { .. } => {
                // Read again under the lock: an accepted version or another
                // override since the preview can make it weaken, or weaken
                // against another version than the password confirmed.
                let manifest = self.engine.current().manifest.clone();
                let set;
                let change = if let Change::ConfirmPackOverride { .. } = change {
                    set = confirm_as_set(store, &manifest, change)?;
                    &set
                } else {
                    change
                };
                let o = override_change(store, &manifest, change)?;
                let bound = o.set.as_ref().map(|s| s.base);
                let sent = match change {
                    Change::SetPackOverride { base, .. } => *base,
                    _ => None,
                };
                // The password confirmed the override against the version
                // the client names back.
                let base_moved = o.set.is_some() && sent != bound;
                if o.weakening && (!out.assessment.confirm || base_moved) {
                    return Err(changed_since_preview());
                }
                out.assessment.pack_base = bound;
                store
                    .set_pack_override(o.rule.id(), o.set.map(|s| s.encode()).as_deref())
                    .map_err(internal)?;
            }
            Change::AcceptPackVersions {
                rules: ids,
                versions,
            } => {
                // Read again under the lock: a shadow result that disagrees,
                // or another version pending, since the preview. The
                // password confirmed the versions the client names back.
                if versions
                    .as_ref()
                    .is_some_and(|v| v.len() > packs::MAX_NAMED)
                {
                    return Err(bad("too_many_versions", "That's more versions than exist.")
                        .field("versions"));
                }
                let manifest = self.engine.current().manifest.clone();
                let acc = packs::acceptance(store, &manifest, ids, now)?;
                let moved = versions.as_ref().is_some_and(|v| {
                    let mut v = v.clone();
                    v.sort();
                    let mut now = acc.versions.clone();
                    now.sort();
                    v != now
                });
                if moved || (acc.weakening && (!out.assessment.confirm || versions.is_none())) {
                    return Err(changed_since_preview());
                }
                // The marks first, so an acceptance never lands without
                // them; put back if the acceptance fails. (The acceptance
                // writes in a transaction of its own, in the engine's
                // crate.) Either way nothing is released.
                let before = packs::add_review(store, &acc.review)?;
                if let Err(e) = pack_upgrade::accept(store, &manifest, &acc.rules, now) {
                    let _ = packs::restore_review(store, before);
                    return Err(internal(e));
                }
                out.assessment.pack_versions = Some(acc.versions);
                out.assessment.pack_acceptance = Some(acc.impact);
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
                // It failed closed while unreadable (lodestar_filter::load):
                // what it held then is released below, to be judged again
                // as fixed, never by the stand-in.
                let was_unreadable = !stored_rule_reads(&row.query)
                    || Action::from_code(row.action).is_none()
                    || row
                        .unless_query
                        .as_deref()
                        .is_some_and(|u| !stored_rule_reads(u));
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
                // Only a change that can fix it, which `assess` already
                // counts as weakening: an enable-only update needs no
                // password, so it never releases.
                if was_unreadable && (q.is_some() || act.is_some() || unless.is_some()) {
                    out.released = Some(self.release_rules(store, &[format!("U-{id}")])?);
                }
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
                // SP-4's release hook (RP-5): an exception for an original
                // removes its signature, so its clones follow. One for a
                // payload covers its clones directly; one by name or query
                // covers the clones it matches, at judging.
                if let RuleRef::Pack(r @ (PackRule::Sp2 | PackRule::Sp3)) = e.rule {
                    match &e.what {
                        Match::Hashes(hs) => {
                            self.release_clones(store, hs, Some(r))?;
                        }
                        Match::Payload(sig) => {
                            self.engine
                                .current()
                                .spam_sigs
                                .release(store, &[*sig], Some(r))
                                .map_err(internal)?;
                        }
                        Match::Keyword(_) | Match::Query(_) => {}
                    }
                }
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
            Change::Subscribe { refresh_hours, .. } => {
                let new = subscriptions::check_subscribe(change)?.ok_or_else(internal_none)?;
                let id = store
                    .add_subscription(&lodestar_store::NewSubscription {
                        url: &new.url,
                        name: &new.name,
                        kind: u8::from(new.ip),
                        format: new.format,
                        action: new.action.code(),
                        floor: new.floor,
                        interval_secs: i64::from(refresh_hours.unwrap_or(24).max(1)) * 3600,
                    })
                    .map_err(internal)?;
                out.id = Some(id);
            }
            Change::Unsubscribe { id } => {
                if !store.remove_subscription(*id).map_err(internal)? {
                    return Err(gone());
                }
                out.released = Some(self.release_list(store, *id)?);
            }
            Change::PauseSubscription { id, enabled } => {
                if !store
                    .set_subscription_enabled(*id, *enabled)
                    .map_err(internal)?
                {
                    return Err(gone());
                }
                if !enabled {
                    out.released = Some(self.release_list(store, *id)?);
                }
            }
            Change::ResolveHeld { id, accept, held } => {
                // Read again under the lock: a refresh since the preview
                // may have held another version. The password confirmed
                // the version the client names, so a loosening accept
                // applies only that one.
                let sub = subscriptions::subscription(store, *id)?;
                let effect = subscriptions::held_effect(store, &sub)?.ok_or_else(nothing_held)?;
                let loosens = *accept && effect.loosens && !sub.floor;
                if loosens
                    && (!out.assessment.confirm || held.as_deref() != Some(effect.version.as_str()))
                {
                    return Err(changed_since_preview());
                }
                let holds = if loosens {
                    subscriptions::dropped_holds(store, *id, &effect.dropped)?.2
                } else {
                    Vec::new()
                };
                if !store.resolve_held(*id, *accept, now).map_err(internal)? {
                    return Err(nothing_held());
                }
                if loosens {
                    // The list's layer swaps first, so nothing released is
                    // held by it again.
                    lodestar_filter::lists::apply(store, &self.engine, &self.ips)
                        .map_err(internal)?;
                    out.released = Some(self.release_list_hashes(
                        store,
                        &subscriptions::rule_id(*id),
                        &holds,
                    )?);
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
        if let Some(before) = sp4_before {
            self.follow_exceptions(store, &before)?;
        }
        self.note_pack_conditions(store, now)?;
        // Best effort, after what must be recorded, and only after a
        // change that can end holds (the start prunes too, for expiries
        // and purges): the row only marks holds, so a line left over marks
        // a hold that's gone, which no page lists.
        if out.released.is_some()
            || matches!(
                change,
                Change::Release { .. }
                    | Change::FalsePositive { .. }
                    | Change::Confirm { .. }
                    | Change::AcceptPackVersions { .. }
                    | Change::Undo
            )
        {
            let _ = packs::prune_review(store);
        }
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
    /// is rebuilt first, so nothing released is held by it again. Returns
    /// how many hashes were released.
    fn release_list(&self, store: &mut Store, id: i64) -> ApiResult<usize> {
        lodestar_filter::lists::apply(store, &self.engine, &self.ips).map_err(internal)?;
        self.release_rules(store, &[subscriptions::rule_id(id)])
    }

    /// Releases `rule`'s holds on `hashes` only (a list version that
    /// dropped them); holds by other rules stay.
    fn release_list_hashes(
        &self,
        store: &mut Store,
        rule: &str,
        hashes: &[[u8; 20]],
    ) -> ApiResult<usize> {
        let mut freed = Vec::new();
        let n = store.release_rule_hashes(rule, hashes, |h| freed.extend_from_slice(h));
        self.unhold(store, &freed)?;
        n.map_err(internal)
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
        // As stored, as the next load reads them.
        let manifest = &engine.manifest;
        let versions = pack_upgrade::PackVersions::resolve(manifest, &pack_upgrade::Rows::of(&set));
        let stored_overrides = PackOverrides::resolve(manifest, &versions, &set.pack_overrides);
        let mut pack_conditions = versions.labels();
        pack_conditions.extend(
            stored_overrides
                .iter()
                .filter(|o| o.unreadable())
                .map(|o| pack_overrides::label(o.rule)),
        );
        // SP-4's signature rows that didn't read at start (RP-5).
        pack_conditions.extend(self.engine.current().spam_sig_labels());
        let pending_on = packs::pending_on(&set, &versions);
        if packs::shadow_in_doubt(&store)? {
            pack_conditions.push(packs::SHADOW_IN_DOUBT.to_owned());
        }
        if packs::review_unreadable(&store)? {
            pack_conditions.push(packs::REVIEW_UNREADABLE.to_owned());
        }
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
            pack_conditions,
            packs: Pack::ALL
                .iter()
                .map(|p| PackState {
                    changelog: packs::lf(pack_manifest::changelog(*p)),
                    id: p.as_str().to_string(),
                    on: engine.packs.on.contains(p),
                    version: packs::pack_version(manifest, *p),
                    rules: PackRule::ALL
                        .iter()
                        .filter(|r| r.pack() == *p)
                        .map(|r| PackRuleState {
                            action: packs::applied_action(
                                manifest,
                                &versions,
                                &stored_overrides,
                                *r,
                            )
                            .as_str()
                            .to_string(),
                            applied: versions
                                .state(*r)
                                .applied
                                .filter(|v| *v != pack_upgrade::UNKNOWN),
                            base_action: pack_overrides::base_action(manifest, &versions, *r)
                                .as_str()
                                .to_string(),
                            base_changed: stored_overrides.get(*r).is_some_and(|o| o.changed),
                            id: r.id().to_string(),
                            on: engine.packs.is_on(*r),
                            pending: pending_on.iter().find(|(p, _)| p == r).map(|(_, v)| *v),
                            version: manifest.entry(*r).current().version,
                            pack_override: stored_overrides.get(*r).map(|o| PackOverrideState {
                                action: o.decoded.action.map(|a| a.as_str().to_string()),
                                base: o.decoded.base,
                                changed: o.changed,
                                paused: o.paused,
                                unless: o.unless_text().map(str::to_owned),
                                unreadable: o.unreadable(),
                            }),
                        })
                        .collect(),
                })
                .collect(),
            packs_seeded_over_rules: versions.seeded_over_rules,
            pending_pack_versions: packs::pending(manifest, &set, &versions),
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
            subscriptions: subs.iter().map(subscriptions::entry).collect(),
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
        let store = self.store();
        let mut t = store
            .tombstones_before(before, page.adult, limit + 1)
            .map_err(internal)?;
        let review = packs::review_set(&store)?;
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
                    pack_review: review.contains(&(t.rule.clone(), t.hash)),
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
        let review = packs::review_set(&store)?;
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
                    pack_review: review.contains(&(q.rule.clone(), q.hash)),
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
        use lodestar_filter::strictness::{self, Candidate};
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
            candidates.push(subscriptions::candidate(&store, s, false)?);
        }
        Ok(subscriptions::to_dto(&strictness::assess(&candidates)))
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
/// The `settings` key holding the re-queues not yet finished: a JSON list
/// of query texts, one per distinct query, written before the rule change
/// and removed once its marks are in.
const REQUEUE_PENDING: &str = "classify.requeue_pending";
/// Set (to `1`) past [`MAX_PENDING`] texts: a full re-mark of every stored
/// torrent, which covers the list. Its own key, so a binary that doesn't
/// know it leaves it alone, and the next start that does still runs it.
const REQUEUE_ALL: &str = "classify.requeue_all";
/// Distinct query texts pending at most. With search off nothing clears
/// them, so an admin changing many different rules would grow the list
/// without limit; past this it collapses into one full re-mark.
const MAX_PENDING: usize = 64;

/// What re-queues are pending.
#[derive(Debug, PartialEq)]
enum Pending {
    /// These query texts' matches.
    Texts(Vec<String>),
    /// Every stored torrent: the texts passed [`MAX_PENDING`].
    All,
}

fn pending_requeues(store: &Store) -> ApiResult<Pending> {
    if store.setting_text(REQUEUE_ALL).map_err(internal)?.is_some() {
        return Ok(Pending::All);
    }
    let text = store.setting_text(REQUEUE_PENDING).map_err(internal)?;
    // Unreadable (never written so): nothing can be recovered from it,
    // and it mustn't block rule changes, so it's started afresh.
    Ok(Pending::Texts(
        text.and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default(),
    ))
}

fn set_pending_requeues(store: &mut Store, pending: &Pending) -> ApiResult<()> {
    match pending {
        // Cleared: both keys, since a full re-mark covers the list too.
        Pending::Texts(list) if list.is_empty() => store
            .update_settings_text(&[], &[REQUEUE_PENDING, REQUEUE_ALL])
            .map_err(internal),
        Pending::Texts(list) => {
            let text = serde_json::to_string(list).map_err(internal)?;
            store
                .set_settings_text(&[(REQUEUE_PENDING, &text)])
                .map_err(internal)
        }
        Pending::All => store
            .update_settings_text(&[(REQUEUE_ALL, "1")], &[REQUEUE_PENDING])
            .map_err(internal),
    }
}

/// Records a re-queue of `query` as pending, before the rule change it
/// follows, so a stop anywhere after the change leaves it to redo. A query
/// already pending isn't added twice, so while there's no search index to
/// clear them the list holds one entry per query text; [`FiltersApi::requeue`] keeps an
/// entry a later rule change still needs. Past [`MAX_PENDING`] texts the
/// list becomes a full re-mark, which covers every query from then on.
/// `true` when it was added.
fn push_pending_requeue(store: &mut Store, query: &str) -> ApiResult<bool> {
    let Pending::Texts(mut list) = pending_requeues(store)? else {
        return Ok(false);
    };
    if list.iter().any(|q| q == query) {
        return Ok(false);
    }
    if list.len() >= MAX_PENDING {
        set_pending_requeues(store, &Pending::All)?;
        return Ok(true);
    }
    list.push(query.to_owned());
    set_pending_requeues(store, &Pending::Texts(list))?;
    Ok(true)
}

/// Clears one pending re-queue of `query` once its marks are in. A full
/// re-mark stays: one query's marks don't cover it.
fn drop_pending_requeue(store: &mut Store, query: &str) -> ApiResult<()> {
    let Pending::Texts(mut list) = pending_requeues(store)? else {
        return Ok(());
    };
    if let Some(i) = list.iter().position(|q| q == query) {
        list.remove(i);
        set_pending_requeues(store, &Pending::Texts(list))?;
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
    /// (§20.8): how many each has and how many it still catches; and the
    /// pack corpora, each rule on its own (RP-1, RP-3).
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
        drop(store);
        Ok(Fixtures {
            core: out,
            packs: packs::corpus(&engine.manifest),
        })
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
        // An always-adult source among its finders: classified Is adult,
        // as the pipeline does, unless you said Not adult. Clearing a
        // correction or setting only Not junk never un-adults it. Only the
        // classification uses it: the stored correction is yours alone.
        let fix = lodestar_filter::reclassify::source_adult(fix, row.sources, self.adult_sources);
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
        let (q, adult) = rules
            .into_iter()
            .find(|r| r.0 == id)
            .map(|r| (r.1, r.2))
            .ok_or_else(missing)?;
        let added = push_pending_requeue(&mut store, &q)?;
        let Some(q) = store.remove_class_rule(id).map_err(internal)? else {
            // Only its own entry: one already there is another change's.
            // If its push collapsed the list to a full re-mark, that stays:
            // deliberately conservative, since the drop never clears it.
            if added {
                drop_pending_requeue(&mut store, &q)?;
            }
            return Err(missing());
        };
        // An unreadable Not adult rule was skipped while loaded, so it
        // marked nothing: nothing to classify again.
        if !adult && !stored_rule_reads(&q) {
            drop_pending_requeue(&mut store, &q)?;
            self.rules_changed(&mut store)?;
            return Ok(ClassRuleRemoved {
                queued: None,
                truncated: false,
            });
        }
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
    /// parses re-queues everything.
    fn requeue_text(&self, text: &str, now: i64) -> ApiResult<(Option<usize>, bool)> {
        #[cfg(test)]
        if tests::FAIL_REQUEUE.with(|f| f.borrow().as_deref() == Some(text)) {
            return Err(internal("injected"));
        }
        // A rule that didn't parse matched everything while it was loaded
        // (an Is adult one, lodestar_filter::load): everything is
        // classified again.
        let node = lodestar_filter::load::rule_query(text).unwrap_or(Node::All);
        self.requeue(&node, text, now)
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

    /// The full re-mark ([`Pending::All`]): marks every stored torrent, and
    /// clears it on the same terms as [`FiltersApi::requeue`] clears a
    /// text: only once an index is there and the marking is all done, with
    /// no rule change since it began. `None` without a search index.
    fn requeue_all(&self) -> ApiResult<Option<usize>> {
        if self.index.is_none() {
            return Ok(None);
        }
        let stop = || {
            if self.shutdown.is_stopped() {
                Err(stopping())
            } else {
                Ok(())
            }
        };
        stop()?;
        let generation = self.store().class_gen().map_err(internal)?;
        // Every id up to the highest stored now, a chunk per lock, with
        // the shutdown checked between chunks. A torrent stored later is
        // enriched under the rules in force by then.
        let last = self.store().max_id().map_err(internal)?;
        let chunk = lodestar_store::MARK_CHUNK as i64;
        let (mut lo, mut n) = (0_i64, 0);
        while lo < last {
            stop()?;
            let hi = lo.saturating_add(chunk).min(last);
            // One range UPDATE per chunk, as `mark_unenriched_after` does.
            n += self
                .store()
                .connection()
                .execute(
                    "UPDATE torrent_parsed SET parser_ver = 0
                     WHERE torrent_id > ?1 AND torrent_id <= ?2",
                    [lo, hi],
                )
                .map_err(internal)?;
            lo = hi;
        }
        self.reenrich.store(true, Ordering::Release);
        #[cfg(test)]
        if let Some(hook) = tests::ALL_MARKED.with(std::cell::Cell::get) {
            hook(self);
        }
        stop()?;
        let mut store = self.store();
        if store.class_gen().map_err(internal)? == generation
            && pending_requeues(&store)? == Pending::All
        {
            set_pending_requeues(&mut store, &Pending::Texts(Vec::new()))?;
        }
        Ok(Some(n))
    }

    /// Redoes the pending re-queues: those a stop cut short, and those
    /// made while search was off (run at every start, in the background;
    /// search is turned on by a restart). Each one an index finishes is
    /// cleared; without an index they all stay. One that fails stays and
    /// the rest still run: its error is logged where it happens, by type
    /// only (`api.internal_error`), never with the query. Only the
    /// shutdown stops the loop, and what's left stays for the next start.
    /// A full re-mark is one re-queue; failed, it stays and its error is
    /// returned. Returns how many an index finished.
    pub fn resume_requeues(&self, now: i64) -> ApiResult<usize> {
        // Pack versions are seeded at the first start that needs it, and
        // their conditions recorded (RP-0). The engine already applies
        // what seeding records, so a failure here loosens nothing: it's
        // logged where it happens and retried by the next change.
        let _ = self.start_packs(now);
        // First what stand-ins of rows that read again did: it may add a
        // full re-mark. Its failure is logged where it happens and leaves
        // them recorded.
        let _ = self.undo_stand_ins(now, false);
        // Read under its own statement: the store guard mustn't live on
        // into the re-queue, which takes the lock itself.
        let pending = pending_requeues(&self.store())?;
        let pending = match pending {
            Pending::All => return Ok(self.requeue_all()?.map_or(0, |_| 1)),
            Pending::Texts(list) => list,
        };
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

/// The core rules' hashed examples, run against the rules as they are,
/// and the pack corpora with counts and disagreements per rule.
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
    let api = f.clone();
    let (removed, requeued_all) = tokio::task::spawn_blocking(move || {
        // An unreadable rule's removal re-queues every stored torrent.
        let unreadable = f
            .store()
            .class_rules()
            .map_err(internal)?
            .iter()
            .any(|r| r.0 == body.id && !stored_rule_reads(&r.1));
        f.remove_class_rule(body.id, crate::unix_now())
            .map(|r| (r, unreadable))
    })
    .await
    .map_err(internal)??;
    // The engine is reloaded: what stand-ins of rows that read again did
    // is judged again, after this request.
    api.undo_in_background(requeued_all);
    Ok(Json(removed))
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
    let api = f.clone();
    let out = tokio::task::spawn_blocking(move || apply_change(&f, &auth, ip, body))
        .await
        .map_err(internal)??;
    // The engine is reloaded: what stand-ins of rows that read again did
    // is judged again, after this request.
    api.undo_in_background(false);
    Ok(out)
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
                // Likewise a held version: the one the warning described.
                if let Change::ResolveHeld {
                    accept: true,
                    held: None,
                    ..
                } = &body.change
                {
                    return Err(bad(
                        "held_needs_version",
                        "Name the version to accept: send back the held_version the warning described as held.",
                    )
                    .field("held"));
                }
                // And an override's base: the version the warning was for.
                if let Change::SetPackOverride { base: None, .. }
                | Change::ConfirmPackOverride { base: None, .. } = &body.change
                {
                    return Err(bad(
                        "pack_override_needs_base",
                        "Name the version the override is for: send back the pack_base the warning was for as base.",
                    )
                    .field("base"));
                }
                // And an acceptance's versions: the ones the warning was for.
                if let Change::AcceptPackVersions { versions: None, .. } = &body.change {
                    return Err(bad(
                        "pack_versions_needed",
                        "Name the versions to accept: send back the pack_versions the warning was for as versions.",
                    )
                    .field("versions"));
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
        /// Runs in the full re-mark once its marks are in, before it clears.
        pub(super) static ALL_MARKED: std::cell::Cell<Option<fn(&FiltersApi)>> =
            const { std::cell::Cell::new(None) };
        /// A pending re-queue text that fails every time it's redone.
        pub(super) static FAIL_REQUEUE: std::cell::RefCell<Option<String>> =
            const { std::cell::RefCell::new(None) };
    }

    fn texts(list: &[&str]) -> Pending {
        Pending::Texts(list.iter().map(|q| (*q).to_owned()).collect())
    }

    /// Below the cap the texts are deduped; past it they collapse into one
    /// full re-mark, which no later push or single-text clear undoes.
    #[test]
    fn pending_requeues_collapse_into_a_full_remark_past_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("t.db"), Default::default()).unwrap();
        let qs: Vec<String> = (0..MAX_PENDING).map(|i| format!("word{i}")).collect();
        for q in &qs {
            assert!(push_pending_requeue(&mut store, q).unwrap());
            assert!(!push_pending_requeue(&mut store, q).unwrap(), "deduped");
        }
        let refs: Vec<&str> = qs.iter().map(String::as_str).collect();
        assert_eq!(pending_requeues(&store).unwrap(), texts(&refs));
        // A repeat at the cap is still deduped, not collapsed.
        assert!(!push_pending_requeue(&mut store, "word0").unwrap());
        assert_eq!(pending_requeues(&store).unwrap(), texts(&refs));
        // One more distinct text: a full re-mark.
        assert!(push_pending_requeue(&mut store, "one-more").unwrap());
        assert_eq!(pending_requeues(&store).unwrap(), Pending::All);
        assert_eq!(
            store.setting_text(REQUEUE_ALL).unwrap().as_deref(),
            Some("1")
        );
        assert_eq!(store.setting_text(REQUEUE_PENDING).unwrap(), None);
        assert!(!push_pending_requeue(&mut store, "later").unwrap());
        // No single text's clear undoes it.
        drop_pending_requeue(&mut store, "word0").unwrap();
        drop_pending_requeue(&mut store, "all").unwrap();
        assert_eq!(pending_requeues(&store).unwrap(), Pending::All);
    }

    /// An API on `store` with an empty in-memory search index.
    fn test_api(store: Store) -> FiltersApi {
        FiltersApi {
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
            adult_sources: 0,
        }
    }

    /// SP-4's release hook (RP-5): an exception for a stored original SP-2
    /// hid, by its hash or its payload, or its release, removes its
    /// signature, in memory and in the store, so its clones follow. An
    /// exception to another rule leaves it.
    #[test]
    fn sp4_clones_follow_an_exception_or_a_release_of_the_original() {
        use lodestar_bencode::{OwnedValue, to_vec};
        use lodestar_filter::spam_sigs::{Original, PREFIX, SpamSigs, usable};
        for how in ["hashes", "payload", "release", "another rule", "undo"] {
            let dir = tempfile::tempdir().unwrap();
            let mut store = Store::open(dir.path().join("t.db"), Default::default()).unwrap();
            let raw = to_vec(&OwnedValue::Dict(
                [
                    (b"length".to_vec(), OwnedValue::Int(1_234_567_891)),
                    (b"name".to_vec(), OwnedValue::Bytes(b"spam.mkv".to_vec())),
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
                    // As SP-2's hide stored it.
                    extra_flags: lodestar_store::flags::HIDDEN,
                    source: lodestar_store::SourceId::new(0).unwrap(),
                }])
                .unwrap();
            let hash = info.primary_hash().0;
            let files = lodestar_classify::file_infos(&info.files, info.path_encoding);
            let sig = usable(lodestar_filter::payload_sig(&files)).unwrap();
            let api = test_api(store);
            let sigs = api.engine.current().spam_sigs.clone();
            // As the writer added it when SP-2 hid the original.
            sigs.add(
                sig,
                Original {
                    rule: PackRule::Sp2,
                    action: lodestar_filter::Action::Hide,
                },
            );
            sigs.flush(&mut api.store()).unwrap();
            let exception = |rule: &str, what: MatchInput| Change::AddException {
                rule: rule.into(),
                what,
            };
            let by_hash = || MatchInput {
                hashes: Some(vec![hex(&hash)]),
                ..MatchInput::default()
            };
            match how {
                "hashes" => apply_change(&api, exception("SP-2", by_hash())),
                "payload" => apply_change(
                    &api,
                    exception(
                        "SP-2",
                        MatchInput {
                            payload: Some(format!("{sig:016x}")),
                            ..MatchInput::default()
                        },
                    ),
                ),
                "release" => apply_change(&api, Change::Release { hash: hex(&hash) }),
                "undo" => {
                    // Excepted, then undone: the signature is back, as the
                    // still-hidden original's. Undone again (redo): gone.
                    apply_change(&api, exception("SP-2", by_hash()));
                    assert!(sigs.get(sig).is_none());
                    apply_change(&api, Change::Undo);
                    assert_eq!(
                        sigs.get(sig),
                        Some(Original {
                            rule: PackRule::Sp2,
                            action: lodestar_filter::Action::Hide,
                        }),
                        "undone"
                    );
                    let again = SpamSigs::default();
                    again.load(&api.store().settings_with_prefix_raw(PREFIX).unwrap());
                    assert!(again.get(sig).is_some(), "undone, after a restart");
                    apply_change(&api, Change::Undo);
                }
                _ => apply_change(&api, exception("SP-3", by_hash())),
            }
            let kept = how == "another rule";
            assert_eq!(sigs.get(sig).is_some(), kept, "{how}");
            // Written: a restart reads the same.
            let again = SpamSigs::default();
            again.load(&api.store().settings_with_prefix_raw(PREFIX).unwrap());
            assert_eq!(again.get(sig).is_some(), kept, "{how} after a restart");
        }
    }

    /// A row of SP-4's set that didn't read at start is listed with the
    /// pack conditions, in `filter.stand_ins` and the overview (RP-5).
    #[test]
    fn an_unreadable_sp4_row_is_listed_with_the_pack_conditions() {
        let api = test_api(Store::open_in_memory(Default::default()).unwrap());
        let rows = vec![(
            format!("{}3", lodestar_filter::spam_sigs::PREFIX),
            Ok("not a row".to_owned()),
        )];
        assert_eq!(api.engine.current().load_spam_sigs(&rows).len(), 1);
        api.start_packs(500).unwrap();
        let record = api.store().setting_text(STAND_INS_KEY).unwrap().unwrap();
        assert!(record.contains("packs.sp4.row.3@500"), "{record}");
        let overview = api.overview().unwrap();
        assert!(
            overview
                .pack_conditions
                .contains(&"packs.sp4.row.3".to_owned()),
            "{:?}",
            overview.pack_conditions
        );
    }

    fn apply_change(api: &FiltersApi, change: Change) {
        let a = api.assess(&change, 1_000).unwrap();
        api.apply(&change, a, 1_000).unwrap();
    }

    fn version_rows(api: &FiltersApi) -> Vec<Option<String>> {
        let store = api.store();
        pack_upgrade::APPLIED_KEYS
            .iter()
            .chain([&pack_upgrade::SEEDED_KEY])
            .map(|k| store.setting_text(k).unwrap())
            .collect()
    }

    fn pack(id: &str, on: bool) -> Change {
        Change::PackSwitch { id: id.into(), on }
    }

    /// RP-0 through the API: seeding before the first change (reported on
    /// a store with rules), SC-1's switch copied to SC-2, a wizard re-run
    /// and Undo/redo accepting nothing, and Undo after an acceptance
    /// leaving the versions.
    #[test]
    fn pack_versions_are_seeded_once_and_undo_leaves_them() {
        let mut store = old_store();
        // As before RP-0: scams on, SC-1 switched off.
        store.set_pack_switch("scams", true).unwrap();
        store.set_pack_switch("SC-1", false).unwrap();
        let api = test_api(store);
        api.start_packs(500).unwrap();
        let seeded = version_rows(&api);
        assert!(seeded.iter().all(Option::is_some), "{seeded:?}");
        assert!(
            api.store()
                .rule_set()
                .unwrap()
                .pack_switches
                .contains(&("SC-2".into(), false))
        );
        assert_eq!(
            api.engine.current().pack_versions.seeded_over_rules,
            Some(500),
            "seeding on a store with rules is reported"
        );
        assert_eq!(api.store().setting_text(STAND_INS_KEY).unwrap(), None);
        assert!(!api.engine.current().packs.is_on(PackRule::Sc2));
        // SC-2 has its own effect warning.
        assert!(api.assess(&pack("SC-2", false), 1_000).unwrap().weakening);
        assert!(!api.assess(&pack("SC-2", true), 1_000).unwrap().weakening);
        // The wizard turns malware on, then runs again: nothing changes.
        apply_change(&api, pack("malware", true));
        let on = version_rows(&api);
        apply_change(&api, pack("malware", true));
        assert_eq!(version_rows(&api), on);
        // Switched off, then Undo and redo: restoring "on" records nothing.
        apply_change(&api, pack("malware", false));
        let malware = || api.engine.current().packs.on.contains(&Pack::Malware);
        assert!(!malware());
        apply_change(&api, Change::Undo);
        assert!(malware());
        apply_change(&api, Change::Undo);
        assert!(!malware());
        apply_change(&api, Change::Undo);
        assert!(malware());
        assert_eq!(version_rows(&api), on);
        // An acceptance, then Undo: the versions stay.
        let accepted = api.accept_pack_versions(&[PackRule::Mw2], 2_000).unwrap();
        let rows = version_rows(&api);
        assert_eq!(rows[0].as_deref(), Some(accepted.encode().as_str()));
        apply_change(&api, Change::Undo);
        assert_eq!(version_rows(&api), rows);
        // The marker still carries the report.
        assert_eq!(rows[2].as_deref(), Some("rules@500"));
    }

    fn widened(c: &lodestar_filter::packs::Ctx<'_>) -> bool {
        c.matches(PackRule::Mw2) || c.name().contains("WIDEN")
    }

    fn real_mw2(c: &lodestar_filter::packs::Ctx<'_>) -> bool {
        c.matches(PackRule::Mw2)
    }

    /// The built-in manifest with MW-2 widened at version 2.
    fn bumped() -> Arc<lodestar_filter::packs::manifest::Manifest> {
        use lodestar_filter::packs::manifest::{Code, Direction, Manifest, RuleEntry, Version};
        let b = Manifest::builtin();
        let v = |version, f: fn(&lodestar_filter::packs::Ctx<'_>) -> bool| Version {
            version,
            direction: Direction::Widens,
            action: Action::Hide,
            code: Some(Code::Fn(f)),
        };
        let mut entries: Vec<RuleEntry> = PackRule::ALL
            .into_iter()
            .filter(|r| *r != PackRule::Mw2)
            .map(|r| b.entry(r).clone())
            .collect();
        entries.push(RuleEntry::new(
            PackRule::Mw2,
            true,
            vec![v(1, real_mw2), v(2, widened)],
        ));
        Arc::new(Manifest::new(entries).unwrap())
    }

    /// A store from before RP-DEF: its creating migration wrote no fresh
    /// marker.
    fn old_store() -> Store {
        let mut store = Store::open_in_memory(Default::default()).unwrap();
        store
            .update_settings_text(&[], &[pack_upgrade::FRESH_KEY])
            .unwrap();
        store
    }

    fn applied_record(api: &FiltersApi) -> pack_upgrade::Applied {
        let rows = version_rows(api);
        assert_eq!(rows[0], rows[1]);
        pack_upgrade::Applied::decode(rows[0].as_deref().unwrap()).unwrap()
    }

    /// RP-DEF: packs that are off on an existing install are offered, not
    /// flipped; turning one on through the preview and apply records its
    /// rules at their current versions, so nothing of it is pending.
    #[test]
    fn turning_a_pack_on_through_preview_and_apply_records_its_current_versions() {
        let api = test_api(old_store());
        api.engine.update_engine(|e| e.manifest = bumped());
        api.start_packs(500).unwrap();
        let e = api.engine.current();
        assert!(!e.packs.on.contains(&Pack::Malware), "not flipped");
        assert_eq!(applied_record(&api).get(PackRule::Mw2), Some(1));
        let change = pack("malware", true);
        let preview = api.assess(&change, 1_000).unwrap();
        assert!(!preview.weakening);
        api.apply(&change, preview, 1_000).unwrap();
        let e = api.engine.current();
        assert!(e.packs.on.contains(&Pack::Malware));
        let rec = applied_record(&api);
        for r in PackRule::ALL
            .into_iter()
            .filter(|r| r.pack() == Pack::Malware)
        {
            let current = e.manifest.entry(r).current().version;
            assert_eq!(rec.get(r), Some(current), "{}", r.id());
            assert!(!e.pack_versions.state(r).pending(), "{}", r.id());
        }
        assert_eq!(rec.get(PackRule::Mw2), Some(2));
        // The other packs stay off, with their explicit rows.
        let set = api.store().rule_set().unwrap();
        assert!(set.pack_switches.contains(&("spam".into(), false)));
    }

    /// RP-DEF: the API's seeding fallback seeds a fresh store as the
    /// pipeline's start does: every pack on at current versions.
    #[test]
    fn the_seeding_fallback_seeds_a_fresh_store_with_every_pack_on() {
        let api = test_api(Store::open_in_memory(Default::default()).unwrap());
        api.engine.update_engine(|e| e.manifest = bumped());
        api.start_packs(500).unwrap();
        let e = api.engine.current();
        assert!(Pack::ALL.iter().all(|p| e.packs.on.contains(p)));
        assert_eq!(applied_record(&api).get(PackRule::Mw2), Some(2));
        assert!(!e.pack_versions.state(PackRule::Mw2).pending());
        // Turning a pack off is weakening, as any other.
        assert!(api.assess(&pack("spam", false), 1_000).unwrap().weakening);
    }

    /// With a version really pending: a wizard re-run, Undo and redo leave
    /// it pending; acceptance switches it, and neither it nor the
    /// stand-in re-judge after it releases a hold the new version
    /// wouldn't make.
    #[test]
    fn a_pending_version_stays_pending_until_accepted_and_nothing_is_released() {
        let store = Store::open_in_memory(Default::default()).unwrap();
        let api = test_api(store);
        api.engine.update_engine(|e| e.manifest = bumped());
        api.start_packs(500).unwrap();
        let pending = || {
            api.engine
                .current()
                .pack_versions
                .state(PackRule::Mw2)
                .pending()
        };
        apply_change(&api, pack("malware", true));
        assert!(!pending(), "off → on records the current version");
        apply_change(&api, pack("MW-2", false));
        // Back to before the switch-on, by hand: MW-2 at version 1 again.
        {
            let mut store = api.store();
            let seed = pack_upgrade::seed_record(&api.engine.current().manifest).encode();
            store
                .set_settings_text(&[
                    (pack_upgrade::APPLIED_KEYS[0], &seed),
                    (pack_upgrade::APPLIED_KEYS[1], &seed),
                ])
                .unwrap();
            api.reload(&store).unwrap();
        }
        assert!(pending());
        // Undo restores MW-2 "on"; redo and Undo again: still pending.
        for _ in 0..3 {
            apply_change(&api, Change::Undo);
        }
        assert!(api.engine.current().packs.is_on(PackRule::Mw2));
        assert!(pending(), "Undo accepts nothing");
        // The wizard runs again with malware on: still pending.
        apply_change(&api, pack("malware", true));
        assert!(pending(), "on → on accepts nothing");
        // Holds made under version 1 that version 2 wouldn't change.
        {
            let store = api.store();
            let c = store.connection();
            c.execute(
                "INSERT INTO quarantine (hash, rule, sealed, at, expires_at)
                 VALUES (?1, 'MW-2', NULL, 1, 9999999999)",
                [vec![1u8; 20]],
            )
            .unwrap();
            c.execute(
                "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'MW-2', 1)",
                [vec![2u8; 20]],
            )
            .unwrap();
        }
        let count = |table: &str| -> i64 {
            api.store()
                .connection()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        };
        api.accept_pack_versions(&[PackRule::Mw2], 2_000).unwrap();
        assert!(!pending());
        let _ = api.undo_stand_ins(2_000, true);
        assert_eq!((count("quarantine"), count("tombstones")), (1, 1));
        // Undo after the acceptance leaves it accepted.
        apply_change(&api, Change::Undo);
        assert!(!pending());
    }

    /// A version-state condition is recorded in `filter.stand_ins`, and
    /// cleared when it's gone; other entries are left alone.
    #[test]
    fn pack_conditions_are_recorded_beside_stand_ins() {
        let mut store = Store::open_in_memory(Default::default()).unwrap();
        store
            .set_settings_text(&[(STAND_INS_KEY, "U-3@100"), (pack_upgrade::SEEDED_KEY, "1")])
            .unwrap();
        let api = test_api(store);
        api.start_packs(700).unwrap();
        let record = || api.store().setting_text(STAND_INS_KEY).unwrap();
        assert_eq!(record().as_deref(), Some("U-3@100 packs.lost@700"));
        // Still lost at the next change: the first time is kept.
        apply_change(&api, pack("malware", true));
        assert_eq!(record().as_deref(), Some("U-3@100 packs.lost@700"));
        api.accept_pack_versions(&PackRule::ALL, 800).unwrap();
        assert_eq!(record().as_deref(), Some("U-3@100"));
    }

    /// An Undo step saved before RP-0 gives SC-2 SC-1's switch and
    /// exceptions, as seeding did.
    #[test]
    fn undo_to_a_set_from_before_rp0_copies_sc1_to_sc2() {
        let mut store = Store::open_in_memory(Default::default()).unwrap();
        let old = json!({
            "rules": [], "overrides": [], "allow": [], "core": [],
            "packs": [["scams", true], ["SC-1", false]],
            "exceptions": [{"id": 4, "rule": "SC-1", "kind": 2, "value": hex(b"robux")}],
        });
        store.save_previous(old.to_string().as_bytes()).unwrap();
        let api = test_api(store);
        apply_change(&api, Change::Undo);
        let set = api.store().rule_set().unwrap();
        assert!(set.pack_switches.contains(&("SC-2".into(), false)));
        let sc2: Vec<_> = set.exceptions.iter().filter(|e| e.rule == "SC-2").collect();
        assert_eq!(sc2.len(), 1);
        assert_eq!(sc2[0].value, b"robux");
        let e = api.engine.current();
        assert!(!e.packs.is_on(PackRule::Sc2));
    }

    /// RP-DEF: an Undo to a rule set saved before RP-DEF (no pack rows)
    /// returns to that set's off state, and nothing seeds the packs on
    /// again at the next start.
    #[test]
    fn undo_to_a_set_from_before_rp_def_is_off_and_stays_off() {
        let api = test_api(Store::open_in_memory(Default::default()).unwrap());
        api.start_packs(500).unwrap();
        assert!(
            Pack::ALL
                .iter()
                .all(|p| api.engine.current().packs.on.contains(p))
        );
        let old = json!({
            "rules": [], "overrides": [], "allow": [], "core": [], "packs": [], "exceptions": [],
        });
        api.store()
            .save_previous(old.to_string().as_bytes())
            .unwrap();
        apply_change(&api, Change::Undo);
        let off = |api: &FiltersApi| {
            let e = api.engine.current();
            assert!(PackRule::ALL.iter().all(|r| !e.packs.is_on(*r)));
            assert!(api.store().rule_set().unwrap().pack_switches.is_empty());
        };
        off(&api);
        api.start_packs(600).unwrap();
        off(&api);
        assert!(
            pack_upgrade::ensure_seeded(
                &mut api.store(),
                &lodestar_filter::packs::manifest::Manifest::builtin(),
                700
            )
            .unwrap()
            .is_none()
        );
        off(&api);
    }

    /// A full re-mark is cleared only if no shutdown and no rule change
    /// came while it marked; otherwise it stays for the next start.
    #[test]
    fn a_full_remark_stopped_or_overtaken_stays_pending() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("t.db"), Default::default()).unwrap();
        set_pending_requeues(&mut store, &Pending::All).unwrap();
        let api = test_api(store);
        // A shutdown during the marking.
        ALL_MARKED.with(|h| h.set(Some(|api| api.shutdown.cancel())));
        assert!(api.resume_requeues(0).is_err());
        assert_eq!(pending_requeues(&api.store()).unwrap(), Pending::All);
        // The next start: the same file, a new shutdown token.
        drop(api);
        let api = test_api(Store::open(dir.path().join("t.db"), Default::default()).unwrap());
        // A rule change during the marking: a new generation.
        ALL_MARKED.with(|h| {
            h.set(Some(|api| {
                api.store().bump_class_gen().unwrap();
            }))
        });
        assert_eq!(api.resume_requeues(0).unwrap(), 1);
        assert_eq!(pending_requeues(&api.store()).unwrap(), Pending::All);
        // An older binary ran meanwhile: it doesn't know the full re-mark's
        // key, so it kept its own list beside it.
        api.store()
            .set_settings_text(&[(REQUEUE_PENDING, r#"["old"]"#)])
            .unwrap();
        assert_eq!(pending_requeues(&api.store()).unwrap(), Pending::All);
        // Undisturbed: done, so cleared, the list it covers too.
        ALL_MARKED.with(|h| h.set(None));
        assert_eq!(api.resume_requeues(0).unwrap(), 1);
        assert_eq!(pending_requeues(&api.store()).unwrap(), texts(&[]));
        assert_eq!(api.store().setting_text(REQUEUE_ALL).unwrap(), None);
        assert_eq!(api.store().setting_text(REQUEUE_PENDING).unwrap(), None);
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
            texts(&["fails", "lexb", "other"])
        );
        let api = test_api(store);
        FAIL_REQUEUE.with(|f| *f.borrow_mut() = Some("fails".into()));
        assert_eq!(api.resume_requeues(0).unwrap(), 2);
        assert_eq!(pending_requeues(&api.store()).unwrap(), texts(&["fails"]));
        // Only the shutdown stops the loop.
        api.shutdown.cancel();
        push_pending_requeue(&mut api.store(), "lexb").unwrap();
        assert!(api.resume_requeues(0).is_err());
        assert_eq!(
            pending_requeues(&api.store()).unwrap(),
            texts(&["fails", "lexb"])
        );
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
            adult_sources: 0,
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
