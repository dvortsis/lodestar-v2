//! `/jobs` (§9, §18.8): the background jobs of this server, with
//! progress, rate, ETA, pause, resume and cancel. Every running export is
//! one (`export.rs`); `import` is reserved for the importer framework.
//!
//! The registry generalizes the filters' preview jobs (#52), which keep
//! their own: at most [`MAX_RUNNING`] running or paused, [`MAX_KEPT`]
//! kept (the oldest finished go first), each finished one kept for
//! [`KEEP`], swept on every start and read. A job whose worker panics
//! (or is dropped unfinished) finishes as failed with a 500 and frees
//! its slot.
//!
//! **Control:** a worker calls [`Job::checkpoint`] between steps. While
//! the job is paused it waits there, holding nothing, until it's
//! resumed, cancelled or the pause runs out; so a worker must never hold
//! a lock or transaction across it.
//!
//! **One registry per router**, so `ApiState` keeps its fields: an
//! `Extension<Arc<Jobs>>` the server layers over the router is used when
//! there is one (so the server can [`Jobs::close`] it at shutdown), and
//! otherwise [`routes`] makes its own.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use tokio::sync::watch;
use utoipa::OpenApi;

use crate::dto::error::ErrorEnvelope;
use crate::dto::jobs::{
    BackgroundJob, BackgroundJobKind, BackgroundJobState, BackgroundJobs, ExportJob,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Path, same_origin_json};
use crate::{ApiState, auth, export};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(
    paths(list_jobs, get_job, pause_job, resume_job, cancel_job),
    tags((name = "jobs", description = "Background jobs: exports now, imports later (§9, §18.8)."))
)]
pub(crate) struct ApiDoc;

/// Jobs running or paused at once, at most.
pub const MAX_RUNNING: usize = 4;
/// Jobs kept at most, running ones included; the oldest finished go first.
pub const MAX_KEPT: usize = 64;
/// How long a finished job is kept.
pub const KEEP: Duration = Duration::from_secs(600);

/// `/jobs` and `/export`, over one registry.
pub fn routes() -> Router<ApiState> {
    let own = Arc::new(Jobs::default());
    auth::protected(
        Router::new()
            .route("/jobs", get(list_jobs))
            .route("/jobs/{id}", get(get_job))
            .route("/jobs/{id}/pause", post(pause_job))
            .route("/jobs/{id}/resume", post(resume_job))
            .route("/jobs/{id}/cancel", post(cancel_job))
            .merge(export::routes()),
    )
    .layer(middleware::from_fn(move |mut req: Request, next: Next| {
        // The server's registry if it gave one, else this router's own.
        if req.extensions().get::<Arc<Jobs>>().is_none() {
            req.extensions_mut().insert(own.clone());
        }
        next.run(req)
    }))
}

/// What a job's controls ask of its worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Run,
    Pause,
    Cancel,
}

/// Why a worker stopped at a checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Cancelled,
    /// Paused for longer than the worker allows.
    PauseExpired,
}

/// How a job ended.
#[derive(Debug)]
enum Outcome {
    Done,
    Cancelled,
    Failed(ApiError),
}

/// Running time, paused time left out, for the rate.
#[derive(Debug)]
struct Clock {
    active: Duration,
    /// When the current running stretch began; `None` while paused.
    since: Option<Instant>,
}

/// One job: its controls and progress, shared by the worker and `/jobs`.
#[derive(Debug)]
pub(crate) struct Job {
    id: u64,
    kind: BackgroundJobKind,
    started_at: i64,
    control: watch::Sender<Control>,
    done: AtomicU64,
    skipped: AtomicU64,
    /// `u64::MAX` while unknown.
    total: AtomicU64,
    total_approx: AtomicBool,
    clock: Mutex<Clock>,
    export: Mutex<Option<ExportJob>>,
    /// The outcome, the instant it was reached and its Unix time.
    finished: Mutex<Option<(Outcome, Instant, i64)>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Job {
    fn new(id: u64, kind: BackgroundJobKind, now: i64) -> Job {
        Job {
            id,
            kind,
            started_at: now,
            control: watch::Sender::new(Control::Run),
            done: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            total: AtomicU64::new(u64::MAX),
            total_approx: AtomicBool::new(false),
            clock: Mutex::new(Clock {
                active: Duration::ZERO,
                since: Some(Instant::now()),
            }),
            export: Mutex::new(None),
            finished: Mutex::new(None),
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Sets the items expected (`approx`: an estimate).
    pub(crate) fn set_total(&self, total: u64, approx: bool) {
        self.total_approx.store(approx, Ordering::Relaxed);
        self.total.store(total.min(u64::MAX - 1), Ordering::Relaxed);
    }

    /// Counts `done` items written and `skipped` left out.
    pub(crate) fn add(&self, done: u64, skipped: u64) {
        self.done.fetch_add(done, Ordering::Relaxed);
        self.skipped.fetch_add(skipped, Ordering::Relaxed);
    }

    pub(crate) fn set_export(&self, export: ExportJob) {
        *lock(&self.export) = Some(export);
    }

    pub(crate) fn mark_truncated(&self) {
        if let Some(e) = lock(&self.export).as_mut() {
            e.truncated = true;
        }
    }

    /// Between steps: returns at once while the job runs; waits while
    /// it's paused, for at most `pause_limit`; stops once it's cancelled.
    pub(crate) async fn checkpoint(&self, pause_limit: Duration) -> Result<(), Stop> {
        let mut rx = self.control.subscribe();
        let paused = match *rx.borrow_and_update() {
            Control::Run => return Ok(()),
            Control::Cancel => return Err(Stop::Cancelled),
            Control::Pause => true,
        };
        debug_assert!(paused);
        let waited = tokio::time::timeout(pause_limit, rx.wait_for(|c| *c != Control::Pause)).await;
        match waited {
            Err(_) => Err(Stop::PauseExpired),
            // The sender lives in the job, so the channel can't close.
            Ok(Err(_)) => Err(Stop::Cancelled),
            Ok(Ok(c)) if *c == Control::Cancel => Err(Stop::Cancelled),
            Ok(Ok(_)) => Ok(()),
        }
    }

    /// Resolves once cancel is asked (for a worker waiting elsewhere,
    /// such as on a slow client).
    pub(crate) async fn cancelled(&self) {
        let mut rx = self.control.subscribe();
        // The sender lives in the job, so the channel can't close.
        let _ = rx.wait_for(|c| *c == Control::Cancel).await;
    }

    pub(crate) fn finish_done(&self) {
        self.finish(Outcome::Done);
    }

    pub(crate) fn finish_cancelled(&self) {
        self.finish(Outcome::Cancelled);
    }

    pub(crate) fn finish_failed(&self, e: ApiError) {
        self.finish(Outcome::Failed(e));
    }

    /// The first outcome stands.
    fn finish(&self, outcome: Outcome) {
        let mut f = lock(&self.finished);
        if f.is_none() {
            self.stop_clock();
            *f = Some((outcome, Instant::now(), crate::unix_now()));
        }
    }

    fn finished_at(&self) -> Option<Instant> {
        lock(&self.finished).as_ref().map(|(_, at, _)| *at)
    }

    fn stop_clock(&self) {
        let mut c = lock(&self.clock);
        if let Some(since) = c.since.take() {
            c.active += since.elapsed();
        }
    }

    fn start_clock(&self) {
        let mut c = lock(&self.clock);
        if c.since.is_none() {
            c.since = Some(Instant::now());
        }
    }

    fn active(&self) -> Duration {
        let c = lock(&self.clock);
        c.active + c.since.map_or(Duration::ZERO, |s| s.elapsed())
    }

    /// Applies a control; refused once the job has finished.
    fn control(&self, to: Control) -> ApiResult<()> {
        // Under the outcome's lock, so a job can't finish in between.
        let f = lock(&self.finished);
        if f.is_some() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "job_finished",
                "That job has already finished.",
            ));
        }
        let from = *self.control.borrow();
        if from == Control::Cancel {
            // Cancelling: nothing undoes it.
            return Ok(());
        }
        match to {
            Control::Pause => self.stop_clock(),
            Control::Run => self.start_clock(),
            Control::Cancel => {}
        }
        self.control.send_replace(to);
        Ok(())
    }

    /// What `/jobs` reports.
    pub(crate) fn status(&self) -> BackgroundJob {
        let finished = lock(&self.finished);
        let control = *self.control.borrow();
        let (state, error, finished_at) = match finished.as_ref() {
            None => (
                match control {
                    Control::Run => BackgroundJobState::Running,
                    Control::Pause => BackgroundJobState::Paused,
                    Control::Cancel => BackgroundJobState::Cancelled,
                },
                None,
                None,
            ),
            Some((o, _, at)) => match o {
                Outcome::Done => (BackgroundJobState::Done, None, Some(*at)),
                Outcome::Cancelled => (BackgroundJobState::Cancelled, None, Some(*at)),
                Outcome::Failed(e) => (BackgroundJobState::Failed, Some(e.error_body()), Some(*at)),
            },
        };
        drop(finished);
        let done = self.done.load(Ordering::Relaxed);
        let skipped = self.skipped.load(Ordering::Relaxed);
        // Skipped items are read too, and `total` counts them.
        let processed = done + skipped;
        let total = match self.total.load(Ordering::Relaxed) {
            u64::MAX => None,
            t => Some(t),
        };
        let secs = self.active().as_secs_f64();
        let rate = (processed > 0 && secs > 0.0).then(|| processed as f64 / secs);
        let eta_s = match (state, rate, total) {
            (BackgroundJobState::Running, Some(r), Some(t)) => {
                Some((t.saturating_sub(processed) as f64 / r).ceil() as u64)
            }
            _ => None,
        };
        BackgroundJob {
            done,
            eta_s,
            error,
            export: lock(&self.export).clone(),
            finished_at,
            id: self.id,
            kind: self.kind,
            rate,
            skipped,
            started_at: self.started_at,
            state,
            total,
            total_approx: self.total_approx.load(Ordering::Relaxed),
        }
    }
}

/// Finishes `job` as failed with a 500 if it's dropped unfinished: its
/// worker panicked, or the runtime dropped it.
pub(crate) struct Finisher(pub(crate) Arc<Job>);

impl Drop for Finisher {
    fn drop(&mut self) {
        if self.0.finished_at().is_none() {
            self.0
                .finish_failed(internal("a background job stopped unfinished"));
        }
    }
}

/// The jobs of this server.
#[derive(Debug, Default)]
pub struct Jobs {
    next: AtomicU64,
    jobs: Mutex<HashMap<u64, Arc<Job>>>,
    closed: AtomicBool,
    pacer: Pacer,
}

/// One read budget shared by every running job, so running several at
/// once reads no faster than one (§1.6.4). Each job books the items it
/// just read on one timeline and waits for its slot, so concurrent jobs
/// share the rate in turn.
#[derive(Debug)]
pub(crate) struct Pacer {
    next: Mutex<tokio::time::Instant>,
}

impl Default for Pacer {
    fn default() -> Self {
        Pacer {
            next: Mutex::new(tokio::time::Instant::now()),
        }
    }
}

impl Pacer {
    /// Books `items` at `per_sec` (0: unpaced) and waits for the time
    /// they're due. Time nobody booked (idle, everything paused, slow
    /// clients) is never banked: booking starts again from now, so no
    /// export can burst to catch up.
    pub(crate) async fn wait(&self, items: u64, per_sec: u32) {
        if per_sec == 0 {
            return;
        }
        let per_item = Duration::from_secs(1) / per_sec;
        let due = {
            let mut next = lock(&self.next);
            let now = tokio::time::Instant::now();
            if *next < now {
                *next = now;
            }
            *next += per_item * u32::try_from(items).unwrap_or(u32::MAX);
            *next
        };
        tokio::time::sleep_until(due).await;
    }
}

impl Jobs {
    pub(crate) fn pacer(&self) -> &Pacer {
        &self.pacer
    }

    /// The map, with the finished jobs past [`KEEP`] swept out.
    fn jobs(&self) -> MutexGuard<'_, HashMap<u64, Arc<Job>>> {
        let mut jobs = lock(&self.jobs);
        jobs.retain(|_, j| j.finished_at().is_none_or(|at| at.elapsed() < KEEP));
        jobs
    }

    /// Cancels every job and refuses new ones (`503 shutting_down`), for
    /// a shutdown: a cancelled export's download ends at once, so open
    /// downloads don't hold the server's graceful shutdown.
    pub fn close(&self) {
        let jobs = {
            let jobs = self.jobs();
            self.closed.store(true, Ordering::Release);
            jobs.values().cloned().collect::<Vec<_>>()
        };
        for job in jobs {
            // A finished one refuses: nothing to do.
            let _ = job.control(Control::Cancel);
        }
    }

    /// Registers a new running job, or 429 when [`MAX_RUNNING`] are.
    pub(crate) fn start(&self, kind: BackgroundJobKind) -> ApiResult<Arc<Job>> {
        let mut jobs = self.jobs();
        if self.closed.load(Ordering::Acquire) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "shutting_down",
                "The server is shutting down.",
            ));
        }
        let running = jobs.values().filter(|j| j.finished_at().is_none()).count();
        if running >= MAX_RUNNING {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_jobs",
                "Too many jobs are running: wait for one to finish, or cancel one in Jobs.",
            ));
        }
        while jobs.len() >= MAX_KEPT {
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
        let job = Arc::new(Job::new(id, kind, crate::unix_now()));
        jobs.insert(id, job.clone());
        Ok(job)
    }

    /// Forgets a job that never got going (its setup failed), freeing its
    /// slot without leaving it in the list.
    pub(crate) fn discard(&self, id: u64) {
        let gone = self.jobs().remove(&id);
        drop(gone);
    }

    pub(crate) fn get(&self, id: u64) -> ApiResult<Arc<Job>> {
        self.jobs().get(&id).cloned().ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "job_not_found",
                "There's no such job.",
            )
        })
    }

    fn list(&self) -> Vec<BackgroundJob> {
        let jobs: Vec<Arc<Job>> = self.jobs().values().cloned().collect();
        let mut out: Vec<BackgroundJob> = jobs.iter().map(|j| j.status()).collect();
        out.sort_by_key(|j| std::cmp::Reverse(j.id));
        out
    }
}

/// Every job kept: running and paused ones, and those finished in the
/// last 10 minutes, newest first.
#[utoipa::path(
    operation_id = "listJobs",
    get,
    path = "/jobs",
    tag = "jobs",
    responses((status = 200, body = BackgroundJobs))
)]
pub(crate) async fn list_jobs(Extension(jobs): Extension<Arc<Jobs>>) -> Json<BackgroundJobs> {
    Json(BackgroundJobs { jobs: jobs.list() })
}

/// One job.
#[utoipa::path(
    operation_id = "getJob",
    get,
    path = "/jobs/{id}",
    tag = "jobs",
    params(("id" = u64, Path, description = "The job's id.")),
    responses(
        (status = 200, body = BackgroundJob),
        (status = 404, body = ErrorEnvelope, description = "`job_not_found`: unknown, or finished more than 10 minutes ago."),
    )
)]
pub(crate) async fn get_job(
    Extension(jobs): Extension<Arc<Jobs>>,
    Path(id): Path<u64>,
) -> ApiResult<Json<BackgroundJob>> {
    Ok(Json(jobs.get(id)?.status()))
}

fn control(
    jobs: &Jobs,
    headers: &HeaderMap,
    id: u64,
    to: Control,
) -> ApiResult<Json<BackgroundJob>> {
    same_origin_json(headers)?;
    let job = jobs.get(id)?;
    job.control(to)?;
    Ok(Json(job.status()))
}

/// Pauses a job: it stops at its next step and holds nothing while it
/// waits (an export's download waits too). Unless it's resumed within
/// 10 minutes it fails with `pause_expired`. Pausing a paused job does
/// nothing.
#[utoipa::path(
    operation_id = "pauseJob",
    post,
    path = "/jobs/{id}/pause",
    tag = "jobs",
    params(("id" = u64, Path, description = "The job's id.")),
    request_body(content = Object, description = "Any JSON, such as `{}`."),
    responses(
        (status = 200, body = BackgroundJob),
        (status = 404, body = ErrorEnvelope, description = "`job_not_found`."),
        (status = 409, body = ErrorEnvelope, description = "`job_finished`."),
    )
)]
pub(crate) async fn pause_job(
    Extension(jobs): Extension<Arc<Jobs>>,
    Path(id): Path<u64>,
    headers: HeaderMap,
) -> ApiResult<Json<BackgroundJob>> {
    control(&jobs, &headers, id, Control::Pause)
}

/// Resumes a paused job. Resuming a running job does nothing.
#[utoipa::path(
    operation_id = "resumeJob",
    post,
    path = "/jobs/{id}/resume",
    tag = "jobs",
    params(("id" = u64, Path, description = "The job's id.")),
    request_body(content = Object, description = "Any JSON, such as `{}`."),
    responses(
        (status = 200, body = BackgroundJob),
        (status = 404, body = ErrorEnvelope, description = "`job_not_found`."),
        (status = 409, body = ErrorEnvelope, description = "`job_finished`."),
    )
)]
pub(crate) async fn resume_job(
    Extension(jobs): Extension<Arc<Jobs>>,
    Path(id): Path<u64>,
    headers: HeaderMap,
) -> ApiResult<Json<BackgroundJob>> {
    control(&jobs, &headers, id, Control::Run)
}

/// Cancels a job: it stops at its next step (a paused one at once). A
/// cancelled export's download ends with an error, so it's never taken
/// for a complete file.
#[utoipa::path(
    operation_id = "cancelJob",
    post,
    path = "/jobs/{id}/cancel",
    tag = "jobs",
    params(("id" = u64, Path, description = "The job's id.")),
    request_body(content = Object, description = "Any JSON, such as `{}`."),
    responses(
        (status = 200, body = BackgroundJob),
        (status = 404, body = ErrorEnvelope, description = "`job_not_found`."),
        (status = 409, body = ErrorEnvelope, description = "`job_finished`."),
    )
)]
pub(crate) async fn cancel_job(
    Extension(jobs): Extension<Arc<Jobs>>,
    Path(id): Path<u64>,
    headers: HeaderMap,
) -> ApiResult<Json<BackgroundJob>> {
    control(&jobs, &headers, id, Control::Cancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: Duration = Duration::from_secs(60);
    const SHORT: Duration = Duration::from_millis(50);

    #[test]
    fn at_most_four_run_at_once_and_finishing_frees_a_slot() {
        let jobs = Jobs::default();
        let started: Vec<_> = (0..MAX_RUNNING)
            .map(|_| jobs.start(BackgroundJobKind::Export).unwrap())
            .collect();
        let err = jobs.start(BackgroundJobKind::Import).unwrap_err();
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(err.code(), "too_many_jobs");
        // A paused job still holds its slot.
        started[1].control(Control::Pause).unwrap();
        assert!(jobs.start(BackgroundJobKind::Export).is_err());
        started[0].finish_done();
        assert!(jobs.start(BackgroundJobKind::Export).is_ok());
    }

    #[test]
    fn finished_jobs_expire_and_are_capped() {
        let jobs = Jobs::default();
        let job = jobs.start(BackgroundJobKind::Export).unwrap();
        job.finish_done();
        assert!(jobs.get(job.id()).is_ok());
        // Ten minutes on, it's gone on read.
        let old = Instant::now()
            .checked_sub(KEEP + Duration::from_secs(1))
            .unwrap();
        lock(&job.finished).as_mut().unwrap().1 = old;
        let err = jobs.get(job.id()).unwrap_err();
        assert_eq!(
            (err.status(), err.code()),
            (StatusCode::NOT_FOUND, "job_not_found")
        );

        // At most 64 kept; the oldest finished go first, running ones stay.
        let running = jobs.start(BackgroundJobKind::Export).unwrap();
        let mut first = None;
        for _ in 0..(MAX_KEPT + 10) {
            let j = jobs.start(BackgroundJobKind::Export).unwrap();
            j.finish_done();
            first.get_or_insert(j.id());
        }
        assert!(jobs.jobs().len() <= MAX_KEPT);
        assert!(
            jobs.get(running.id()).is_ok(),
            "a running job is never evicted"
        );
        assert!(
            jobs.get(first.unwrap()).is_err(),
            "the oldest finished went"
        );
        // Newest first.
        let list = jobs.list();
        assert!(list.windows(2).all(|w| w[0].id > w[1].id));
    }

    #[test]
    fn a_panicking_worker_fails_its_job_with_a_500_and_frees_its_slot() {
        let jobs = Jobs::default();
        let job = jobs.start(BackgroundJobKind::Export).unwrap();
        let worker = job.clone();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _finisher = Finisher(worker);
            panic!("boom");
        }));
        assert!(caught.is_err());
        let st = jobs.get(job.id()).unwrap().status();
        assert_eq!(st.state, BackgroundJobState::Failed);
        assert_eq!(st.error.unwrap().code, "internal");
        assert!(st.finished_at.is_some());
        for _ in 0..MAX_RUNNING {
            jobs.start(BackgroundJobKind::Export).unwrap();
        }
    }

    #[test]
    fn the_first_outcome_stands_and_finished_jobs_refuse_controls() {
        let jobs = Jobs::default();
        let job = jobs.start(BackgroundJobKind::Export).unwrap();
        job.finish_done();
        job.finish_failed(internal("late"));
        assert_eq!(job.status().state, BackgroundJobState::Done);
        for c in [Control::Pause, Control::Run, Control::Cancel] {
            let err = job.control(c).unwrap_err();
            assert_eq!(
                (err.status(), err.code()),
                (StatusCode::CONFLICT, "job_finished")
            );
        }
    }

    #[tokio::test]
    async fn checkpoints_wait_while_paused_and_stop_when_cancelled() {
        let jobs = Jobs::default();
        let job = jobs.start(BackgroundJobKind::Export).unwrap();
        assert_eq!(job.checkpoint(LIMIT).await, Ok(()));

        job.control(Control::Pause).unwrap();
        assert_eq!(job.status().state, BackgroundJobState::Paused);
        let waiter = {
            let job = job.clone();
            tokio::spawn(async move { job.checkpoint(LIMIT).await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!waiter.is_finished(), "a paused job waits");
        job.control(Control::Run).unwrap();
        assert_eq!(waiter.await.unwrap(), Ok(()));

        // A pause that runs out.
        job.control(Control::Pause).unwrap();
        assert_eq!(job.checkpoint(SHORT).await, Err(Stop::PauseExpired));

        // Cancel wins over a pause, and nothing undoes it.
        let waiter = {
            let job = job.clone();
            tokio::spawn(async move { job.checkpoint(LIMIT).await })
        };
        tokio::task::yield_now().await;
        job.control(Control::Cancel).unwrap();
        assert_eq!(waiter.await.unwrap(), Err(Stop::Cancelled));
        job.control(Control::Run).unwrap();
        assert_eq!(*job.control.borrow(), Control::Cancel);
        assert_eq!(job.checkpoint(LIMIT).await, Err(Stop::Cancelled));
        assert_eq!(job.status().state, BackgroundJobState::Cancelled);
        job.finish_cancelled();
        assert_eq!(job.status().state, BackgroundJobState::Cancelled);
        assert!(job.status().finished_at.is_some());
    }

    #[tokio::test]
    async fn closing_cancels_every_job_and_refuses_new_ones() {
        let jobs = Jobs::default();
        let running = jobs.start(BackgroundJobKind::Export).unwrap();
        let paused = jobs.start(BackgroundJobKind::Export).unwrap();
        paused.control(Control::Pause).unwrap();
        let done = jobs.start(BackgroundJobKind::Export).unwrap();
        done.finish_done();
        let waiter = {
            let paused = paused.clone();
            tokio::spawn(async move { paused.checkpoint(LIMIT).await })
        };
        tokio::task::yield_now().await;
        jobs.close();
        assert_eq!(waiter.await.unwrap(), Err(Stop::Cancelled));
        assert_eq!(running.checkpoint(LIMIT).await, Err(Stop::Cancelled));
        assert_eq!(done.status().state, BackgroundJobState::Done);
        let err = jobs.start(BackgroundJobKind::Export).unwrap_err();
        assert_eq!(
            (err.status(), err.code()),
            (StatusCode::SERVICE_UNAVAILABLE, "shutting_down")
        );
    }

    #[tokio::test]
    async fn the_pacer_shares_one_rate_and_never_bursts() {
        let jobs = Arc::new(Jobs::default());
        let start = Instant::now();
        // Two jobs, 200 items each, at 1,000 a second between them.
        let tasks: Vec<_> = (0..2)
            .map(|_| {
                let jobs = jobs.clone();
                tokio::spawn(async move {
                    for _ in 0..10 {
                        jobs.pacer().wait(20, 1000).await;
                    }
                })
            })
            .collect();
        for t in tasks {
            t.await.unwrap();
        }
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(390), "{took:?}");
        assert!(took < Duration::from_secs(3), "{took:?}");
        // Idle time, short or long, is never banked: after it the pacer
        // paces from now, with no burst to catch up.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let resumed = Instant::now();
        jobs.pacer().wait(100, 1000).await;
        let took = resumed.elapsed();
        assert!(
            took >= Duration::from_millis(95),
            "a short idle stretch was banked: {took:?}"
        );
        // After a long idle stretch it paces from now, not from where it was.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let resumed = Instant::now();
        jobs.pacer().wait(100, 1000).await;
        let took = resumed.elapsed();
        assert!(took >= Duration::from_millis(95), "{took:?}");
        assert!(took < Duration::from_millis(600), "{took:?}");
        let free = Instant::now();
        jobs.pacer().wait(1_000_000, 0).await;
        assert!(free.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn rate_and_eta_leave_paused_time_out() {
        let jobs = Jobs::default();
        let job = jobs.start(BackgroundJobKind::Export).unwrap();
        let st = job.status();
        assert_eq!((st.rate, st.eta_s, st.total), (None, None, None));
        job.set_total(1000, false);
        // As if it had run for 2 s.
        {
            let mut c = lock(&job.clock);
            c.since = None;
            c.active = Duration::from_secs(2);
        }
        job.add(97, 3);
        let st = job.status();
        // Skipped items count as read: 100 in 2 s, 900 left.
        assert_eq!(st.rate, Some(50.0));
        assert_eq!(st.eta_s, Some(18));
        assert_eq!((st.done, st.skipped, st.total), (97, 3, Some(1000)));
        job.control(Control::Pause).unwrap();
        assert_eq!(job.status().eta_s, None, "no ETA while paused");
        assert_eq!(job.status().rate, Some(50.0), "paused time doesn't count");
    }
}
