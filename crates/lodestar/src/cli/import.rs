//! `lodestar import`: bring a Magnetico or Bitmagnet database into the
//! store (IMP-4, `docs/decisions/2026-10-31-IMP-4b.md`).
//!
//! - `inspect` only reads: the source's identity, what it holds, a row
//!   estimate and whether the data folder has room.
//! - `start`, `resume`, `pause`, `cancel` and `remove` write the store, so
//!   they run while the server is stopped: they hold the store
//!   exclusively, and refuse while anything else has it open. The import
//!   then goes through the store's one writer, this process.
//! - `status` only reads, and is safe while the server runs.
//!
//! Ctrl-C during an import pauses it after the batch in flight (or
//! cancels that batch's read): `resume` carries on from the last
//! committed batch.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{Args, Subcommand, ValueEnum};
use lodestar_filter::{Engine, Sealer};
use lodestar_pipeline::StoreFilter;
use lodestar_sources_importers::driver::{self, Adapter, Ended, Report};
use lodestar_sources_importers::{bitmagnet, magnetico};
use lodestar_store::Store;
use lodestar_store::import::{
    Control, ImportCounters, ImportJob, NewImportJob, Phase, Reason, Removed, Revalidation,
};
use lodestar_store::rusqlite::ErrorCode;
use serde_json::json;

use super::util::{
    Failure, Outcome, dir_bytes, failed, format_bytes, format_count, refuse_during_update_rollback,
    store_bytes, unix_now,
};
use crate::config::Config;

/// What an imported torrent costs on disk, store and index together, until
/// the store holds enough torrents to measure it: the 100k measurement's
/// figure, rounded up (decisions file).
pub const DEFAULT_RECORD_BYTES: u64 = 4096;
/// Torrents a store needs before its own size per torrent is trusted.
const MEASURE_FROM: u64 = 10_000;
/// Headroom on top of the estimate: the WAL, the index's merges.
const HEADROOM_PCT: u64 = 25;
/// Records a removal step takes back in one transaction.
const REMOVE_STEP: usize = 5_000;

#[derive(Debug, Args, PartialEq, Eq)]
pub struct ImportArgs {
    #[command(subcommand)]
    pub action: Action,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Action {
    /// Check a source without importing: what it is, how many torrents it
    /// holds, and whether there's room (only reads; safe while the server
    /// runs)
    Inspect {
        #[command(flatten)]
        source: SourceArgs,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Import a source into a new job (server stopped)
    Start {
        #[command(flatten)]
        source: SourceArgs,
        /// Pause after about this many torrents (a whole batch of 500 at a
        /// time), to try a pilot first
        #[arg(long, value_name = "N")]
        limit: Option<u64>,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Show the import jobs, or one job in detail (safe while the server
    /// runs)
    Status {
        /// The job's number
        job: Option<i64>,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Pause a blocked job so it waits for you (server stopped)
    Pause { job: i64 },
    /// Carry on with a paused or blocked job, after checking its source is
    /// unchanged (server stopped)
    Resume {
        job: i64,
        #[command(flatten)]
        source: SourceArgs,
        /// Pause again after about this many more torrents
        #[arg(long, value_name = "N")]
        limit: Option<u64>,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Stop a job for good; what it imported stays (server stopped)
    Cancel { job: i64 },
    /// Take back the torrents a cancelled or finished job imported (server
    /// stopped); asks first unless --yes
    Remove {
        job: i64,
        /// Remove without asking
        #[arg(long)]
        yes: bool,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SourceKind {
    Magnetico,
    Bitmagnet,
}

/// Which source, and where. The password comes from --password-file or
/// LODESTAR_IMPORT_PG_PASSWORD, never the command line.
#[derive(Debug, Args, PartialEq, Eq)]
pub struct SourceArgs {
    #[arg(long, value_enum)]
    pub source: SourceKind,
    /// Magnetico: the database file (database.sqlite3); stop magneticod
    /// first, or point at a copy
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Magnetico: a network share the database may be on, like
    /// \\nas\share (repeat for more); shares are refused otherwise
    #[arg(long = "allow-share", value_name = "PATH")]
    pub allow_shares: Vec<PathBuf>,
    /// Bitmagnet: PostgreSQL's host name or IP address, or its Unix socket
    /// folder [default: localhost]
    #[arg(long)]
    pub host: Option<String>,
    /// Bitmagnet: PostgreSQL's port [default: 5432]
    #[arg(long)]
    pub port: Option<u16>,
    /// Bitmagnet: the database [default: bitmagnet]
    #[arg(long)]
    pub dbname: Option<String>,
    /// Bitmagnet: the read-only user [default: bitmagnet_reader]
    #[arg(long)]
    pub user: Option<String>,
    /// Bitmagnet: read the password from this file (chmod 600)
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,
    /// Bitmagnet: trust only the CA certificates in this PEM file for TLS
    #[arg(long, value_name = "PATH")]
    pub ca_file: Option<PathBuf>,
    /// Bitmagnet: connect without TLS, allowed only to a Unix socket or a
    /// loopback address
    #[arg(long, conflicts_with = "ca_file")]
    pub allow_plaintext_loopback: bool,
}

impl SourceArgs {
    /// The Bitmagnet connection flags given, by name.
    fn pg_flags(&self) -> Vec<&'static str> {
        [
            ("--host", self.host.is_some()),
            ("--port", self.port.is_some()),
            ("--dbname", self.dbname.is_some()),
            ("--user", self.user.is_some()),
            ("--password-file", self.password_file.is_some()),
            ("--ca-file", self.ca_file.is_some()),
            ("--allow-plaintext-loopback", self.allow_plaintext_loopback),
        ]
        .into_iter()
        .filter_map(|(name, given)| given.then_some(name))
        .collect()
    }

    /// The source's own checks before anything is opened: each adapter's
    /// flags with its own source only.
    fn check(&self) -> Outcome {
        let bad = |m: String| Err(Failure::Failed(m));
        match self.source {
            SourceKind::Magnetico => {
                if self.path.is_none() {
                    return bad("--source magnetico needs --path to its database file".into());
                }
                if let Some(flag) = self.pg_flags().first() {
                    return bad(format!("{flag} is for --source bitmagnet"));
                }
            }
            SourceKind::Bitmagnet => {
                if self.path.is_some() || !self.allow_shares.is_empty() {
                    return bad(
                        "--path and --allow-share are for --source magnetico; Bitmagnet is \
                         reached with --host, --port, --dbname and --user"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }

    fn bitmagnet_settings(&self) -> Outcome<bitmagnet::Settings> {
        let mut s = bitmagnet::Settings::new(
            self.host.as_deref().unwrap_or("localhost"),
            self.dbname.as_deref().unwrap_or("bitmagnet"),
            self.user.as_deref().unwrap_or("bitmagnet_reader"),
        );
        s.port = self.port.unwrap_or(5432);
        s.tls = if self.allow_plaintext_loopback {
            bitmagnet::Tls::PlaintextLoopback
        } else {
            bitmagnet::Tls::Verify {
                ca_file: self.ca_file.clone(),
            }
        };
        s.password = match &self.password_file {
            Some(path) => {
                if let Some(warning) = bitmagnet::password_file_warning(path) {
                    eprintln!("lodestar import: {warning}");
                }
                Some(
                    bitmagnet::Password::from_file(path)
                        .map_err(failed(format!("reading {}", path.display())))?,
                )
            }
            None => bitmagnet::Password::from_env(),
        };
        Ok(s)
    }
}

pub fn run(config: &Config, args: ImportArgs) -> Outcome {
    refuse_during_update_rollback(&config.data_dir)?;
    match args.action {
        Action::Inspect { source, json } => block_on(inspect(config, &source, json)),
        Action::Start {
            source,
            limit,
            json,
        } => block_on(start(config, &source, limit, json, None)),
        Action::Status { job, json } => status(config, job, json),
        Action::Pause { job } => control(config, job, "paused", |s, j| {
            s.pause_import_job(j.id, j.revision, unix_now())
        }),
        Action::Resume {
            job,
            source,
            limit,
            json,
        } => block_on(resume(config, job, &source, limit, json)),
        Action::Cancel { job } => control(config, job, "cancelled", |s, j| {
            s.cancel_import_job(j.id, j.revision, unix_now())
        }),
        Action::Remove { job, yes, json } => remove(config, job, yes, json),
    }
}

/// `lodestar import-bitmagnet`: the old command, now an import job. A
/// cursor file it left is bound to the new job once, then set aside.
pub fn legacy(config: &Config, args: super::ImportBitmagnetArgs) -> Outcome {
    refuse_during_update_rollback(&config.data_dir)?;
    eprintln!(
        "lodestar import-bitmagnet is now `lodestar import start --source bitmagnet` (same \
         connection flags; `lodestar import inspect` checks first). Running that."
    );
    let source = SourceArgs {
        source: SourceKind::Bitmagnet,
        path: None,
        allow_shares: Vec::new(),
        host: Some(args.host),
        port: Some(args.port),
        dbname: Some(args.dbname),
        user: Some(args.user),
        password_file: args.password_file,
        ca_file: args.ca_file,
        allow_plaintext_loopback: args.allow_plaintext_loopback,
    };
    let cursor = config.data_dir.join("import-bitmagnet.cursor");
    block_on(async {
        // Run again, the old command carried on: so does its new form.
        let unfinished = {
            let store = open_exclusive(config)?;
            let opened = open_source(&source).await?;
            job_for(&store, &opened.adapter)?.map(|j| j.id)
        };
        match unfinished {
            Some(id) => {
                eprintln!("Carrying on with job {id}.");
                resume(config, id, &source, args.limit, false).await
            }
            None => start(config, &source, args.limit, false, Some(&cursor)).await,
        }
    })
}

fn block_on(f: impl Future<Output = Outcome>) -> Outcome {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(failed("starting the runtime"))?
        .block_on(f)
}

/// The store, held exclusively until it's dropped: the import is then the
/// store's only writer. Refused while the server or another command has
/// it open, and on a data folder Lodestar hasn't set up yet.
fn open_exclusive(config: &Config) -> Outcome<Store> {
    let db = config.db_path();
    if !db.is_file() {
        return Err(Failure::Refused(format!(
            "there's no store at {} yet. Start Lodestar once so it sets the store up, stop it, \
             then import.",
            db.display()
        )));
    }
    let in_use = || {
        Failure::Refused(format!(
            "the store {} is open in another process. Imports run while Lodestar is stopped: \
             stop it (and any other lodestar command), import, then start it again.",
            db.display()
        ))
    };
    let store = match Store::open(&db, config.store_config()) {
        Ok(s) => s,
        Err(lodestar_store::Error::Sqlite(e))
            if matches!(
                e.sqlite_error_code(),
                Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
            ) =>
        {
            return Err(in_use());
        }
        Err(e) => return Err(Failure::Failed(format!("opening {}: {e}", db.display()))),
    };
    let take = |c: &lodestar_store::rusqlite::Connection| {
        c.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
        // A write takes the lock, which the locking mode then keeps.
        c.execute_batch("BEGIN IMMEDIATE; COMMIT;")
    };
    match take(store.connection()) {
        Ok(()) => Ok(store),
        Err(e)
            if matches!(
                e.sqlite_error_code(),
                Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
            ) =>
        {
            Err(in_use())
        }
        Err(e) => Err(Failure::Failed(format!("locking {}: {e}", db.display()))),
    }
}

/// An opened source and what it says about its size.
struct Opened {
    adapter: Adapter,
    /// Torrent rows: counted (Magnetico) or PostgreSQL's estimate.
    rows: Option<u64>,
    counted: bool,
}

async fn open_source(args: &SourceArgs) -> Outcome<Opened> {
    args.check()?;
    match args.source {
        SourceKind::Magnetico => {
            let path = args.path.as_deref().expect("checked");
            let policy = magnetico::OpenPolicy {
                approved_unc_roots: args.allow_shares.clone(),
                ..magnetico::OpenPolicy::default()
            };
            let src = magnetico::open(path, &policy).map_err(refused)?;
            Ok(Opened {
                rows: Some(src.rows()),
                counted: true,
                adapter: Adapter::Magnetico(src),
            })
        }
        SourceKind::Bitmagnet => {
            let settings = args.bitmagnet_settings()?;
            let src = bitmagnet::Source::open(&settings).await.map_err(refused)?;
            let rows = src.estimate_rows().await.map_err(refused)?;
            Ok(Opened {
                rows,
                counted: false,
                adapter: Adapter::Bitmagnet(src),
            })
        }
    }
}

/// A source refusal: nothing was changed, and the message names the safe
/// step.
fn refused(r: impl Into<driver::SourceRefusal>) -> Failure {
    let r = r.into();
    Failure::Refused(format!("{} [{}]", r.message, r.label))
}

/// Room for the source in the data folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Space {
    per_record: u64,
    measured: bool,
    needed: Option<u64>,
    free: Option<u64>,
}

impl Space {
    fn short(&self) -> bool {
        matches!((self.needed, self.free), (Some(n), Some(f)) if n > f)
    }
}

/// What `rows` more torrents need, by this store's own size per torrent
/// once it holds enough, and the data folder's free space.
fn space(config: &Config, store: &Store, rows: Option<u64>) -> Space {
    let stored = store.count().unwrap_or(0);
    let used = store_bytes(&config.db_path()) + config.index_path().parent().map_or(0, dir_bytes);
    let (per_record, measured) = if stored >= MEASURE_FROM {
        ((used / stored).max(1), true)
    } else {
        (DEFAULT_RECORD_BYTES, false)
    };
    Space {
        per_record,
        measured,
        needed: rows.map(|n| {
            n.saturating_mul(per_record)
                .saturating_mul(100 + HEADROOM_PCT)
                / 100
        }),
        free: free_bytes(&config.data_dir),
    }
}

/// Bytes free to this user on `dir`'s volume (Unix); unknown elsewhere.
fn free_bytes(dir: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let s = rustix::fs::statvfs(dir).ok()?;
        s.f_bavail.checked_mul(s.f_frsize)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

fn short_of_space(s: &Space) -> Failure {
    Failure::Refused(format!(
        "not enough free space: the import needs about {} in the data folder and {} is free. \
         Free some space or move the data folder, then try again.",
        format_bytes(s.needed.unwrap_or(0)),
        format_bytes(s.free.unwrap_or(0))
    ))
}

fn hex8(b: &[u8]) -> String {
    b.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// The unfinished job already bound to this source identity, if any.
fn job_for(store: &Store, adapter: &Adapter) -> Outcome<Option<ImportJob>> {
    let (dataset, schema_sig) = adapter.identity();
    let mut before = None;
    loop {
        let page = store
            .import_jobs(before, 100)
            .map_err(failed("reading the import jobs"))?;
        let Some(last) = page.last() else {
            return Ok(None);
        };
        before = Some(last.id);
        if let Some(j) = page.into_iter().find(|j| {
            !j.is_finished()
                && j.adapter == adapter.name()
                && j.dataset == dataset
                && j.schema_sig == schema_sig
        }) {
            return Ok(Some(j));
        }
    }
}

async fn inspect(config: &Config, args: &SourceArgs, json: bool) -> Outcome {
    let opened = open_source(args).await?;
    let db = config.db_path();
    let store = if db.is_file() {
        Some(Store::open_reader(&db, config.store_config()).map_err(failed("opening the store"))?)
    } else {
        None
    };
    let space = store.as_ref().map(|s| space(config, s, opened.rows));
    let existing = match &store {
        Some(s) => job_for(s, &opened.adapter)?,
        None => None,
    };
    let (dataset, schema_sig) = opened.adapter.identity();
    let caps = match &opened.adapter {
        Adapter::Bitmagnet(s) => {
            let c = s.caps();
            vec![
                ("files_status", c.files_status),
                ("files_count", c.files_count),
                ("pieces", c.pieces != bitmagnet::Pieces::None),
            ]
        }
        Adapter::Magnetico(_) => vec![("files", true)],
    };
    if json {
        let out = json!({
            "source": opened.adapter.name(),
            "adapter_version": opened.adapter.version(),
            "dataset": hex8(&dataset),
            "schema": hex8(&schema_sig),
            "capabilities": caps.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>(),
            "rows": opened.rows,
            "rows_counted": opened.counted,
            "bytes_per_torrent": space.map(|s| s.per_record),
            "bytes_per_torrent_measured": space.map(|s| s.measured),
            "bytes_needed": space.and_then(|s| s.needed),
            "bytes_free": space.and_then(|s| s.free),
            "enough_space": space.map(|s| !s.short()),
            "existing_job": existing.as_ref().map(|j| j.id),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(failed("encoding"))?
        );
    } else {
        println!(
            "Source: {} (adapter version {}), dataset {}…, schema {}…",
            opened.adapter.name(),
            opened.adapter.version(),
            hex8(&dataset),
            hex8(&schema_sig)
        );
        let caps: Vec<String> = caps
            .iter()
            .map(|(k, v)| format!("{k} {}", if *v { "yes" } else { "no" }))
            .collect();
        println!("Has: {}", caps.join(", "));
        match opened.rows {
            Some(n) if opened.counted => println!("Torrents: {}", format_count(n)),
            Some(n) => println!(
                "Torrents: about {} (PostgreSQL's estimate)",
                format_count(n)
            ),
            None => println!("Torrents: unknown (PostgreSQL hasn't analysed the table yet)"),
        }
        match space {
            None => println!("Space: no store yet; start Lodestar once first."),
            Some(s) => println!(
                "Space: needs about {} ({} a torrent, {}), {} free",
                s.needed.map_or("an unknown amount".into(), format_bytes),
                format_bytes(s.per_record),
                if s.measured {
                    "measured on this store"
                } else {
                    "a typical figure"
                },
                s.free.map_or("an unknown amount".into(), format_bytes)
            ),
        }
        if let Some(j) = &existing {
            println!(
                "Job {} already imports this source ({}). Carry on with `lodestar import \
                 resume {}` and the same source flags.",
                j.id,
                state(j),
                j.id
            );
        }
    }
    match space {
        Some(s) if s.short() => Err(short_of_space(&s)),
        _ => Ok(()),
    }
}

async fn start(
    config: &Config,
    args: &SourceArgs,
    limit: Option<u64>,
    json: bool,
    legacy_cursor: Option<&Path>,
) -> Outcome {
    let mut store = open_exclusive(config)?;
    let mut opened = open_source(args).await?;
    if let Some(j) = job_for(&store, &opened.adapter)? {
        return Err(Failure::Refused(format!(
            "job {} already imports this source ({}). Carry on with `lodestar import resume {}` \
             and the same source flags.",
            j.id,
            state(&j),
            j.id
        )));
    }
    let s = space(config, &store, opened.rows);
    if s.short() {
        return Err(short_of_space(&s));
    }
    let a = &opened.adapter;
    let (dataset, schema_sig) = a.identity();
    // The old command's cursor is checked before the job exists, so a
    // refusal leaves nothing behind for a rerun to resume past it.
    let legacy = match legacy_cursor {
        Some(path) => check_legacy_cursor(&store, path, &dataset)?.map(|p| (path, p)),
        None => None,
    };
    let now = unix_now();
    let job = store
        .create_import_job(&NewImportJob {
            adapter: a.name(),
            adapter_ver: a.version(),
            dataset: &dataset,
            schema_sig: &schema_sig,
            options: if legacy.is_some() { LEGACY_BOUND } else { b"" },
            credential: None,
            at: now,
        })
        .map_err(failed("creating the import job"))?;
    if let Some((path, position)) = legacy {
        bind_legacy_cursor(&mut store, &job, path, &position)?;
    }
    eprintln!("Import job {} started.", job.id);
    run_job(config, &mut store, job.id, &mut opened.adapter, limit, json).await
}

/// The options of a job a legacy cursor file was bound to.
const LEGACY_BOUND: &[u8] = b"legacy-cursor";

/// The legacy cursor file's position, if there's one for this dataset,
/// checked before any job is created. A file from another database is
/// refused, never skipped past (IMP-3); so is one already bound to a job
/// (a crash between binding and setting the file aside left it), which
/// would skip what that job imported.
fn check_legacy_cursor(store: &Store, path: &Path, dataset: &[u8; 32]) -> Outcome<Option<Vec<u8>>> {
    let position = bitmagnet::legacy_cursor(path, dataset).map_err(|e| {
        Failure::Refused(format!(
            "{} ({}): delete it to start over; torrents already imported are kept and \
             recognised",
            e,
            path.display()
        ))
    })?;
    if position.is_empty() {
        return Ok(None);
    }
    let mut before = None;
    loop {
        let page = store
            .import_jobs(before, 100)
            .map_err(failed("reading the import jobs"))?;
        if let Some(bound) = page.iter().find(|j| {
            j.adapter == bitmagnet::ADAPTER && j.dataset == dataset && j.options == LEGACY_BOUND
        }) {
            return Err(Failure::Refused(format!(
                "{} was already bound to job {}: delete it, then run the import again",
                path.display(),
                bound.id
            )));
        }
        match page.last() {
            Some(last) => before = Some(last.id),
            None => return Ok(Some(position)),
        }
    }
}

/// Binds a checked legacy position to the new job as its first batch,
/// with no records, then sets the file aside so it's bound once.
fn bind_legacy_cursor(store: &mut Store, job: &ImportJob, path: &Path, position: &[u8]) -> Outcome {
    let now = unix_now();
    let j = store
        .set_import_phase(job.id, job.revision, Phase::Ready, now)
        .and_then(|j| store.set_import_phase(j.id, j.revision, Phase::Reading, now))
        .map_err(failed("starting the import job"))?;
    let source = store
        .register_source(bitmagnet::ADAPTER, env!("CARGO_PKG_VERSION"), now)
        .map_err(failed("registering the source"))?;
    store
        .commit_import_batch(&lodestar_store::import::ImportBatch {
            job: j.id,
            seq: 1,
            position,
            source,
            source_bytes: 0,
            at: now,
            class_gen: None,
            records: &[],
        })
        .map_err(failed("binding the cursor"))?;
    let mut bound = path.as_os_str().to_os_string();
    bound.push(format!(".bound-to-job-{}", j.id));
    std::fs::rename(path, &bound).map_err(failed(format!("setting {} aside", path.display())))?;
    eprintln!(
        "Carrying on from where import-bitmagnet stopped (its cursor file is now {}).",
        PathBuf::from(bound).display()
    );
    Ok(())
}

async fn resume(
    config: &Config,
    id: i64,
    args: &SourceArgs,
    limit: Option<u64>,
    json: bool,
) -> Outcome {
    let mut store = open_exclusive(config)?;
    let job = load(&store, id)?;
    let mut opened = open_source(args).await?;
    if job.adapter != opened.adapter.name() {
        return Err(Failure::Failed(format!(
            "job {id} imports from {}, not {}",
            job.adapter,
            opened.adapter.name()
        )));
    }
    if job.control != Control::Running {
        let (dataset, schema_sig) = opened.adapter.identity();
        // What's left to read, by the job's own share of the source.
        let left = opened.rows.map(|n| n.saturating_sub(job.counters.read));
        let s = space(config, &store, left);
        let resumed = store
            .resume_import_job(
                id,
                job.revision,
                &Revalidation {
                    dataset: &dataset,
                    schema_sig: &schema_sig,
                    credential_ok: true,
                    space_ok: !s.short(),
                },
                unix_now(),
            )
            .map_err(|e| Failure::Failed(format!("job {id}: {e}")))?;
        if let Some(reason) = resumed.reason {
            return Err(Failure::Failed(format!(
                "job {id} can't carry on: {}",
                explain(reason)
            )));
        }
    }
    run_job(config, &mut store, id, &mut opened.adapter, limit, json).await
}

/// The filter the server would judge with, built the way it builds it
/// (`filter_load`): the store's floor, subscribed lists, CS-1 entries and
/// switches, packs and your rules. The classification rules' generation
/// is read first, as the writer does.
pub(crate) fn filter(config: &Config, store: &mut Store) -> Outcome<(Arc<Engine>, i64)> {
    let class_gen = store.class_gen().map_err(failed("reading the rules"))?;
    let files = config
        .blocklist_files()
        .map_err(failed("reading the blocklist files"))?;
    let handle = lodestar_pipeline::default_filter();
    crate::filter_load::sync_sources(
        store,
        &config.subscriptions,
        &files,
        &handle,
        &Default::default(),
        unix_now(),
    )
    .map_err(Failure::Failed)?;
    let from = StoreFilter::read(store).map_err(failed("reading the filter"))?;
    let skipped = lodestar_pipeline::load_filter(&handle, &from);
    if !skipped.is_empty() {
        eprintln!(
            "lodestar import: {} filter rule(s) couldn't be read and are skipped, as the server \
             skips them",
            skipped.len()
        );
    }
    Ok((handle.current(), class_gen))
}

/// The seal key the server uses for quarantined names, if it has made
/// one; without it a quarantine keeps no name. Never created here.
fn sealer(config: &Config) -> Option<Sealer> {
    match crate::filter_load::read_seal_key(&config.data_dir.join("seal.key")) {
        Ok(key) => key.map(|k| Sealer::new(&k)),
        Err(e) => {
            eprintln!("lodestar import: {e}: quarantined names won't be kept");
            None
        }
    }
}

async fn run_job(
    config: &Config,
    store: &mut Store,
    id: i64,
    adapter: &mut Adapter,
    limit: Option<u64>,
    json: bool,
) -> Outcome {
    let (engine, class_gen) = filter(config, store)?;
    let sealer = sealer(config);
    let quarantine_secs = config
        .pipeline_config()
        .map_err(failed("reading the config"))?
        .quarantine
        .as_secs() as i64;
    let source = store
        .register_source(adapter.name(), env!("CARGO_PKG_VERSION"), unix_now())
        .map_err(failed("registering the source"))?;
    let stop = Arc::new(AtomicBool::new(false));
    {
        let (stop, cancel) = (stop.clone(), adapter.canceller());
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("Pausing at the last saved batch… (Ctrl-C again to quit now)");
                stop.store(true, Ordering::Relaxed);
                cancel.store(true, Ordering::Relaxed);
            }
            // A second one quits at once: every committed batch is safe,
            // and the job resumes from the last one.
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!(
                    "Stopped. Nothing is lost: `lodestar import resume` carries on from the last \
                     saved batch."
                );
                std::process::exit(130);
            }
        });
    }
    let started = std::time::Instant::now();
    let progress = |c: &ImportCounters| {
        if !json && c.read % 50_000 < magnetico::MAX_PARENTS as u64 {
            eprintln!(
                "{} read, {} new, {} a second",
                format_count(c.read),
                format_count(c.new),
                format_count((c.read as f64 / started.elapsed().as_secs_f64().max(1e-3)) as u64)
            );
        }
    };
    let opts = driver::Options {
        source,
        filter: driver::Filter {
            engine: &engine,
            sealer: sealer.as_ref(),
            quarantine_secs,
            class_gen,
        },
        stop: &stop,
        limit,
        now: unix_now,
        progress: Some(&progress),
    };
    let report = driver::run(store, id, adapter, &opts)
        .await
        .map_err(|e| Failure::Failed(format!("job {id}: {e}")))?;
    let _ = store.checkpoint();
    print_report(id, &report, json)?;
    match report.ended {
        Ended::Exhausted | Ended::Paused => Ok(()),
        Ended::Blocked(reason) => Err(Failure::Failed(format!(
            "job {id} is blocked: {}{}",
            explain(reason),
            refusal_text(&report)
        ))),
        Ended::Failed => Err(Failure::Failed(format!(
            "job {id} failed: the source can't be imported{}",
            refusal_text(&report)
        ))),
        Ended::NotRunning(c) => Err(Failure::Failed(format!(
            "job {id} is {}: it stopped after {} batch(es) this run, all of them saved",
            c.label(),
            report.batches
        ))),
    }
}

fn refusal_text(r: &Report) -> String {
    r.refusal
        .as_ref()
        .map_or(String::new(), |r| format!(" ({} [{}])", r.message, r.label))
}

fn explain(reason: Reason) -> &'static str {
    match reason {
        Reason::SourceChanged => {
            "the source isn't the one the job started on (it changed, or it's another \
             database). Start a new job for it"
        }
        Reason::SourceUnavailable => "the source can't be read right now",
        Reason::NoSpace => "there isn't enough free space in the data folder",
        Reason::NeedsCredentials => "it needs its credentials",
        Reason::Interrupted => "Lodestar stopped while it ran",
        Reason::StoreError => "the store reported an error",
    }
}

fn counters_json(c: &ImportCounters) -> serde_json::Value {
    json!({
        "read": c.read, "new": c.new, "merged": c.merged, "candidates": c.candidates,
        "filtered": c.filtered, "quarantined": c.quarantined, "invalid": c.invalid,
        "files_dropped": c.files_dropped, "partial": c.partial, "source_bytes": c.source_bytes,
    })
}

fn print_report(id: i64, r: &Report, json: bool) -> Outcome {
    let ended = match r.ended {
        Ended::Exhausted => "done",
        Ended::Paused => "paused",
        Ended::Blocked(_) => "blocked",
        Ended::Failed => "failed",
        Ended::NotRunning(_) => "not_running",
    };
    if json {
        let out = json!({
            "job": id,
            "ended": ended,
            "refusal": r.refusal.as_ref().map(|r| r.label),
            "this_run": { "batches": r.batches, "records": r.records },
            "counters": counters_json(&r.counters),
            "findings": r.findings,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(failed("encoding"))?
        );
        return Ok(());
    }
    let c = &r.counters;
    println!(
        "Job {id}: {} read this run; {} read in all: {} new, {} already stored, {} waiting for \
         details, {} filtered, {} quarantined, {} invalid.",
        format_count(r.records),
        format_count(c.read),
        format_count(c.new),
        format_count(c.merged),
        format_count(c.candidates),
        format_count(c.filtered),
        format_count(c.quarantined),
        format_count(c.invalid)
    );
    if !r.findings.is_empty() {
        let found: Vec<String> = r
            .findings
            .iter()
            .map(|(k, v)| format!("{k} {}", format_count(*v)))
            .collect();
        println!("Found this run: {}", found.join(", "));
    }
    match r.ended {
        Ended::Exhausted => println!(
            "The source is fully imported. Start Lodestar: it indexes the new torrents and \
             re-fetches incomplete file lists."
        ),
        Ended::Paused => println!(
            "Paused. Carry on with `lodestar import resume {id}` and the same source flags."
        ),
        _ => {}
    }
    Ok(())
}

fn load(store: &Store, id: i64) -> Outcome<ImportJob> {
    store
        .import_job(id)
        .map_err(failed("reading the import job"))?
        .ok_or_else(|| Failure::Failed(format!("there's no import job {id}")))
}

/// A job's state in a few words.
fn state(j: &ImportJob) -> String {
    match (j.control, j.reason) {
        (Control::Running, _) => j.phase.label().replace('_', " "),
        (c, Some(r)) => format!("{}: {}", c.label(), r.label()),
        (c, None) => c.label().into(),
    }
}

fn status(config: &Config, id: Option<i64>, json: bool) -> Outcome {
    let db = config.db_path();
    if !db.is_file() {
        return Err(Failure::Failed(format!("no store at {}", db.display())));
    }
    let store =
        Store::open_reader(&db, config.store_config()).map_err(failed("opening the store"))?;
    let jobs = match id {
        Some(id) => vec![load(&store, id)?],
        None => store
            .import_jobs(None, 50)
            .map_err(failed("reading the import jobs"))?,
    };
    if json {
        let out: Vec<_> = jobs
            .iter()
            .map(|j| {
                json!({
                    "job": j.id, "source": j.adapter, "phase": j.phase.label(),
                    "control": j.control.label(), "reason": j.reason.map(Reason::label),
                    "counters": counters_json(&j.counters), "waiting_candidates": j.open_candidates,
                    "created_at": j.created_at, "updated_at": j.updated_at,
                    "finished_at": j.finished_at,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(failed("encoding"))?
        );
        return Ok(());
    }
    if jobs.is_empty() {
        println!("No import jobs yet. Check a source with `lodestar import inspect`.");
    }
    for j in &jobs {
        let c = &j.counters;
        println!(
            "Job {} ({}): {}. {} read: {} new, {} already stored, {} waiting for details, {} \
             filtered, {} quarantined, {} invalid.",
            j.id,
            j.adapter,
            state(j),
            format_count(c.read),
            format_count(c.new),
            format_count(c.merged),
            format_count(c.candidates),
            format_count(c.filtered),
            format_count(c.quarantined),
            format_count(c.invalid)
        );
        if j.phase == Phase::Indexing && j.control == Control::Running {
            println!(
                "  Waiting for the search index: the job completes once the running server has \
                 indexed these torrents."
            );
        }
    }
    Ok(())
}

/// One job change (pause, cancel), server stopped.
fn control(
    config: &Config,
    id: i64,
    done: &str,
    f: impl FnOnce(&mut Store, &ImportJob) -> lodestar_store::Result<ImportJob>,
) -> Outcome {
    let mut store = open_exclusive(config)?;
    let job = load(&store, id)?;
    let changed = f(&mut store, &job).map_err(|e| Failure::Failed(format!("job {id}: {e}")))?;
    println!("Job {id} is {done}: {}.", state(&changed));
    Ok(())
}

fn remove(config: &Config, id: i64, yes: bool, json: bool) -> Outcome {
    let mut store = open_exclusive(config)?;
    let job = load(&store, id)?;
    let reading = !matches!(job.control, Control::Cancelled | Control::Failed)
        && job.phase.code() <= Phase::Committing.code();
    if reading {
        return Err(Failure::Refused(format!(
            "job {id} can still read its source: cancel it first with `lodestar import cancel \
             {id}`, so it can't later resume past what's removed"
        )));
    }
    if !yes {
        use std::io::IsTerminal;
        eprintln!(
            "This takes back the torrents job {id} imported ({} new). Torrents another job, the \
             crawler or you have used since are kept. Back up first with `lodestar backup`.",
            format_count(job.counters.new)
        );
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            return Err(Failure::Refused(
                "no terminal to confirm on; add --yes to remove without asking".into(),
            ));
        }
        eprint!("Type yes to remove: ");
        let _ = std::io::Write::flush(&mut std::io::stderr());
        let mut line = String::new();
        if stdin.read_line(&mut line).is_err() || !line.trim().eq_ignore_ascii_case("yes") {
            eprintln!("Nothing was changed.");
            return Ok(());
        }
    }
    let r = remove_all(&mut store, id)?;
    let _ = store.checkpoint();
    if json {
        let out = json!({
            "job": id, "deleted": r.torrents, "kept_shared": r.kept_shared,
            "kept_verified": r.kept_verified, "kept_seen": r.kept_seen, "kept_yours": r.kept_yours,
            "provenance_removed": r.provenance, "candidates_deleted": r.candidates,
            "private_candidates_kept": r.kept_private_candidates,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(failed("encoding"))?
        );
    } else {
        println!(
            "Removed job {id}'s torrents: {} deleted, {} kept because something else uses them, \
             {} others no longer list this job. Lodestar drops the deleted ones from search \
             when it next starts.",
            format_count(r.torrents),
            format_count(r.kept_shared + r.kept_verified + r.kept_seen + r.kept_yours),
            format_count(r.provenance)
        );
        if r.kept_private_candidates > 0 {
            println!(
                "{} private torrent(s) still waiting for details were kept: another import may \
                 have marked them private.",
                format_count(r.kept_private_candidates)
            );
        }
    }
    Ok(())
}

/// Every step of a removal, until nothing of the job's is left.
fn remove_all(store: &mut Store, id: i64) -> Outcome<Removed> {
    let mut total = Removed::default();
    while !total.done {
        let step = store
            .remove_import_job_records(id, REMOVE_STEP)
            .map_err(|e| Failure::Failed(format!("job {id}: {e}")))?;
        total.add(step);
    }
    Ok(total)
}

#[cfg(test)]
mod tests;
