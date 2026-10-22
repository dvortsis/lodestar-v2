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

use std::collections::HashMap;
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
    Action, CoreRule, Doc, Engine, Exception, Facts, Fixture, Handle, HeldSet, Item, Match, Origin,
    Override, Pack, PackRule, RuleRef, Sealer, Strength,
};
use lodestar_search::{Node, parse};
use lodestar_store::{ExceptionRow, OverrideRow, RuleRow, RuleSet, Store};
use serde_json::{Value, json};
use utoipa::OpenApi;

use crate::ApiState;
use crate::auth::{self, auth_error};
use crate::dto::auth::OkResponse;
use crate::dto::filters::{
    AllowEntry, ApplyResponse, ClassRule, ClassRuleAdded, ClassRuleRemoved, Classes, CoreRuleState,
    CorrectResponse, Decision, DecisionCount, Detection, DetectionExample, ExceptionEntry,
    FixtureCounts, Fixtures, JobPhase, JobProgress, JobStarted, JobState, JobStatus, OverrideEntry,
    Overview, PackRuleState, PackState, QuarantineEntry, Queue, RuleEntry, SubscriptionEntry,
    TestResponse, TestResult,
};
pub use crate::dto::filters::{
    Assessment, Change, ChangeBody, ClassRuleBody, CorrectBody, IdBody, MatchInput, Preview,
    Sample, SampleFile, TestBody,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{ClientIp, Json as JsonBody, Path, same_origin_json};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(
    overview,
    queue,
    fixtures,
    test,
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
    pub index: Option<lodestar_search::SearchIndex>,
    /// Starts another background-enrichment pass in the pipeline.
    pub reenrich: Arc<AtomicBool>,
    pub jobs: PreviewJobs,
}

/// The replay window (§20.10).
const REPLAY_SECS: i64 = 30 * 86_400;
/// Rows read for a preview.
const PREVIEW_ROWS: usize = 100_000;

/// The filters routes (§20.7): all behind a session.
pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/filters", get(overview))
            .route("/filters/queue", get(queue))
            .route("/filters/fixtures", get(fixtures))
            .route("/filters/test", post(test))
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

/// The preview jobs of this server; finished ones are kept for
/// [`JOB_KEEP`].
#[derive(Debug, Default)]
pub struct PreviewJobs {
    next: AtomicU64,
    jobs: Mutex<HashMap<u64, Arc<Job>>>,
}

/// Previews running at once, at most.
const MAX_JOBS: usize = 4;
/// How long a finished preview's result is kept.
const JOB_KEEP: Duration = Duration::from_secs(600);

impl PreviewJobs {
    fn start(&self) -> ApiResult<(u64, Arc<Job>)> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.retain(|_, j| {
            j.finished
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none_or(|at| at.elapsed() < JOB_KEEP)
        });
        let running = jobs
            .values()
            .filter(|j| {
                j.finished
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_none()
            })
            .count();
        if running >= MAX_JOBS {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_previews",
                "Too many previews are running: wait for one to finish, or cancel it.",
            ));
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let job = Arc::new(Job::default());
        jobs.insert(id, job.clone());
        Ok((id, job))
    }

    fn get(&self, id: u64) -> ApiResult<Arc<Job>> {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned()
            .ok_or_else(|| not_found("preview_not_found", "There's no such preview."))
    }
}

/// The current state, read once per preview.
struct Snapshot {
    quarantine: Vec<lodestar_store::Quarantined>,
    tombstones: Vec<([u8; 20], String, i64)>,
    hits: Vec<(String, u8, i64)>,
}

impl Snapshot {
    fn read(store: &Store, now: i64) -> lodestar_store::Result<Snapshot> {
        Ok(Snapshot {
            quarantine: store.quarantined(PREVIEW_ROWS)?,
            tombstones: store.tombstones(PREVIEW_ROWS)?.into_iter().collect(),
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
            quarantine: self.quarantine.iter().filter(|q| hit(&q.rule)).count(),
            tombstones: self.tombstones.iter().filter(|t| hit(&t.1)).count(),
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
                .filter(|(h, rule, _)| released_by_hash(after, h, rule))
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

/// The effect warning (§20.11): what it does, what else it lets
/// through, the numbers, and a safer alternative.
fn warning(change: &Change, p: &Preview, neutral: Option<&Preview>) -> String {
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
             checked. It will be fetched and indexed. {n}"
        ),
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
                if !on && engine.core.switches.is_on(r) {
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
                a.preview.tombstones = snap.tombstones.iter().filter(|t| hs.contains(&t.0)).count();
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
                a.preview.tombstones = snap.tombstones.iter().filter(|t| t.0 == h).count();
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
                if *accept && shrank {
                    a.weakening = true;
                    a.preview = rules(vec![format!("L-{id}")]);
                }
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
            a.warning = Some(warning(change, &a.preview, a.neutralize.as_ref()));
        }
        progress.begin(DONE, 0);
        Ok(a)
    }

    /// Reloads the rule layers and switches from the store.
    fn reload(&self, store: &Store) -> ApiResult<()> {
        let set = store.rule_set().map_err(internal)?;
        let class_rules = store.class_rules().map_err(internal)?;
        let state = store.filter_state().map_err(internal)?;
        for note in self.engine.update_engine(|e| {
            for (rule, on) in &state.switches {
                if let Some(r) = CoreRule::from_id(rule) {
                    let _ = e.core.switches.set(r, *on);
                }
            }
            let mut notes = e.load_rules(&set);
            notes.extend(e.load_class_rules(&class_rules));
            notes
        }) {
            tracing::warn!("filters: {note}");
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
        self.held.remove(&hash);
        Ok(had)
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
                    store.set_core_switch(r.id(), *on).map_err(internal)?;
                    if !on {
                        // Its quarantine is released and processed (§20.5.3).
                        let held: Vec<[u8; 20]> = store
                            .quarantined(PREVIEW_ROWS)
                            .map_err(internal)?
                            .into_iter()
                            .filter(|q| q.rule == r.id())
                            .map(|q| q.hash)
                            .collect();
                        for h in &held {
                            self.release(store, *h)?;
                        }
                        out.released = Some(held.len());
                    }
                }
            }
            Change::PackSwitch { id, on } => {
                pack_rules(id)?;
                store.set_pack_switch(id, *on).map_err(internal)?;
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
                for h in &hs {
                    self.release(store, *h)?;
                }
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
                if !store.resolve_held(*id, *accept, now).map_err(internal)? {
                    return Err(conflict("nothing_held", "No version of that list is held."));
                }
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
        ) {
            lodestar_filter::lists::apply(store, &self.engine, &self.ips).map_err(internal)?;
        }
        self.reload(store)?;
        out.version = self.engine.current().version;
        Ok(out)
    }

    /// A list that stopped applying lets its tombstones go.
    fn release_list(&self, store: &mut Store, id: i64) -> ApiResult<()> {
        let rule = format!("L-{id}");
        let dropped: Vec<[u8; 20]> = store
            .tombstones(PREVIEW_ROWS)
            .map_err(internal)?
            .into_iter()
            .filter(|t| t.1 == rule)
            .map(|t| t.0)
            .collect();
        for h in dropped {
            self.release(store, h)?;
        }
        Ok(())
    }

    /// The rule set, switches and lists, as the Filters & Safety screens
    /// show them.
    pub fn overview(&self) -> ApiResult<Overview> {
        let engine = self.engine.current();
        let store = self.store();
        let set = store.rule_set().map_err(internal)?;
        let subs = store.subscriptions().map_err(internal)?;
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

    /// The review queue (§20.7): quarantine, never with names, and the
    /// replay window's decisions per rule.
    pub fn queue(&self, now: i64) -> ApiResult<Queue> {
        let store = self.store();
        let q = store.quarantined(1_000).map_err(internal)?;
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
            quarantine: q
                .iter()
                .map(|q| QuarantineEntry {
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
/// Stored torrents one classification rule change re-queues at most.
const MAX_REQUEUE: usize = 1_000_000;

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
                    tracing::warn!(rule = r.id(), "unreadable fixture skipped");
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
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let engine = self.engine.current();
        let (enriched, c) = lodestar_filter::reclassify::enrich_row(
            &row,
            &files,
            fix,
            Some(&engine),
            crate::unix_now(),
        );
        store.set_enrichments(&[(id, enriched)]).map_err(internal)?;
        Ok(CorrectResponse {
            adult_kind: c.adult_kind.map(|k| k.as_str().to_string()),
            junk: c.junk.score,
            content_type: c.content_type.as_str().to_string(),
        })
    }

    /// Your classification rules and how detection compares with your
    /// per-torrent corrections (§20.9): each correction is a fixture for
    /// adult detection, and the ones detection gets wrong are listed (up to
    /// 50) so a rule can cover their kind.
    pub fn classes(&self) -> ApiResult<Classes> {
        let mut store = self.store();
        let rules = store.class_rules().map_err(internal)?;
        let corrections = store.corrections(MAX_CORRECTIONS).map_err(internal)?;
        let (mut checked, mut wrong) = (0, Vec::new());
        let mut disagree = 0;
        for (id, c) in &corrections {
            let Some(adult) = c.adult else { continue };
            let (Some(row), Some(tree)) = (
                store.torrent(*id).map_err(internal)?,
                store.files(*id).map_err(internal)?,
            ) else {
                continue;
            };
            let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
            let detected =
                lodestar_classify::classify(&row.name, &files).content_type == ContentType::Adult;
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
        Ok(Classes {
            corrections: corrections.len(),
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
        let mut store = self.store();
        let id = store
            .add_class_rule(&node.to_query(), body.adult)
            .map_err(internal)?;
        self.reload(&store)?;
        let queued = self.requeue(&mut store, &node, now)?;
        Ok(ClassRuleAdded { id, queued })
    }

    /// Removes a classification rule; the torrents it matched are
    /// classified again without it.
    pub fn remove_class_rule(&self, id: i64, now: i64) -> ApiResult<ClassRuleRemoved> {
        let mut store = self.store();
        let q = store
            .remove_class_rule(id)
            .map_err(internal)?
            .ok_or_else(|| {
                not_found(
                    "class_rule_not_found",
                    "There's no such classification rule.",
                )
            })?;
        self.reload(&store)?;
        let queued = match parse(&q) {
            Ok(node) => self.requeue(&mut store, &node, now)?,
            Err(_) => None,
        };
        Ok(ClassRuleRemoved { queued })
    }

    /// Marks the stored torrents `node` matches for background enrichment.
    /// Without a search index, only torrents arriving or re-enriched later
    /// are affected (`null`).
    fn requeue(&self, store: &mut Store, node: &Node, now: i64) -> ApiResult<Option<usize>> {
        let Some(index) = &self.index else {
            return Ok(None);
        };
        let compiled = index.query(node, now);
        let ids: Vec<i64> = index
            .matching_ids(&compiled, MAX_REQUEUE)
            .map_err(internal)?
            .into_iter()
            .map(|id| id as i64)
            .collect();
        let n = store.mark_unenriched(&ids).map_err(internal)?;
        self.reenrich.store(true, Ordering::Release);
        Ok(Some(n))
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
#[utoipa::path(get, path = "/filters", tag = "filters", responses((status = 200, body = Overview)))]
pub(crate) async fn overview(State(s): State<ApiState>) -> ApiResult<Json<Overview>> {
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.overview().map(Json))
        .await
        .map_err(internal)?
}

/// The review queue: quarantine (never with names) and the last 30 days'
/// decisions per rule.
#[utoipa::path(get, path = "/filters/queue", tag = "filters", responses((status = 200, body = Queue)))]
pub(crate) async fn queue(State(s): State<ApiState>) -> ApiResult<Json<Queue>> {
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.queue(crate::unix_now()).map(Json))
        .await
        .map_err(internal)?
}

/// The core rules' hashed examples, run against the rules as they are.
#[utoipa::path(
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

/// Your classification correction for one stored torrent; it's
/// classified again at once.
#[utoipa::path(
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
#[utoipa::path(get, path = "/classify", tag = "classify", responses((status = 200, body = Classes)))]
pub(crate) async fn classes(State(s): State<ApiState>) -> ApiResult<Json<Classes>> {
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.classes().map(Json))
        .await
        .map_err(internal)?
}

/// Adds a classification rule; the stored torrents it matches are
/// classified again.
#[utoipa::path(
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
        let result = f.assess_tracked(&body.change, now, &job.progress);
        *job.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        *job.finished.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    });
    Ok((StatusCode::ACCEPTED, Json(JobStarted { job: id })))
}

/// A preview job: progress while it runs, then the assessment (or the
/// error) once it's done.
#[utoipa::path(
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
    let job = filters(&s)?.jobs.get(id)?;
    let result = job.result.lock().unwrap_or_else(|e| e.into_inner());
    let progress = job.progress.snapshot();
    let (state, assessment, error) = match result.as_ref() {
        None => (JobState::Running, None, None),
        Some(Ok(a)) => (JobState::Done, Some(a.clone()), None),
        Some(Err(e)) => (JobState::Failed, None, Some(e)),
    };
    Ok(Json(JobStatus {
        assessment,
        error: error.map(ApiError::error_body),
        progress,
        state,
        status: error.map(|e| e.status().as_u16()),
    }))
}

/// Cancels a preview job; it stops at its next step.
#[utoipa::path(
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
            Some(p) => auth.confirm(ip, p).map_err(|e| auth_error(e, "password"))?,
        }
    }
    f.apply(&body.change, assessment, now).map(Json)
}
