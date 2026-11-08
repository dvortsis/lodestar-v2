//! Windows limits (plan §4 CMP-4b, Q3 (b)).
//!
//! **One Job Object for every companion,** with kill-on-job-close. Lodestar
//! holds the only handle, so when Lodestar exits for any reason the
//! kernel closes it and kills every process in the job, including what a
//! companion started: children are created inside their parent's job.
//!
//! **The assignment race.** CMP-4a's spawn doesn't create the process
//! suspended, so [`JobHook`] assigns it right after `CreateProcess`
//! returns. A companion that started a child of its own inside that
//! window (before its loader has finished, microseconds) would leave the
//! child outside the job. qBittorrent and Prowlarr start none at start-up;
//! the window is stated in `docs/decisions/2026-11-08-CMP-4b.md` rather
//! than closed.
//!
//! **No memory cap:** `win32job` sets only the working-set size, which
//! trims memory rather than capping it, and setting the job's process
//! memory limit needs `unsafe`. The report says so.
//!
//! **No sandbox:** there's no safe, maintained restricted-token or
//! AppContainer wrapper, so the report says the companion could read
//! Lodestar's data (Q3 (b): the confirmation states it).

use win32job::{ExtendedLimitInfo, Job};

use super::limits::{Applied, Level, Limits, LimitsError, Mechanism, Report, Sandboxed};
use super::process::AfterSpawn;

/// The Job Object and the hook that assigns each companion to it.
pub struct JobHook {
    job: Job,
}

impl std::fmt::Debug for JobHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobHook").finish_non_exhaustive()
    }
}

impl JobHook {
    pub fn new() -> Result<JobHook, LimitsError> {
        let mut info = ExtendedLimitInfo::new();
        info.limit_kill_on_job_close();
        let job = Job::create_with_limit_info(&info).map_err(|_| LimitsError::NoJob)?;
        Ok(JobHook { job })
    }

    /// The pids in the job.
    pub fn members(&self) -> Vec<u32> {
        self.job
            .query_process_id_list()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|p| u32::try_from(p).ok())
            .collect()
    }
}

impl AfterSpawn for JobHook {
    fn applied(&self, child: &tokio::process::Child) -> Result<(), &'static str> {
        // `raw_handle` is `None` once Tokio has reaped it.
        let handle = child.raw_handle().ok_or("job_assign_failed")?;
        self.job
            .assign_process(handle as isize)
            .map_err(|_| "job_assign_failed")
    }
}

/// What every Windows companion gets.
pub(crate) fn report(_: &Limits) -> Report {
    Report {
        sandbox: Sandboxed {
            level: Level::None,
            reason: "no_safe_wrapper",
        },
        limits: Applied {
            mechanism: Mechanism::JobObject,
            open_files: None,
            memory: None,
            dies_with_lodestar: true,
            cgroup_skipped: None,
        },
    }
}
