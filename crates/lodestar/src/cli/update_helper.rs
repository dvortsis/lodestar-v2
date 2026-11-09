//! `lodestar update-helper` (hidden): the MSI's update helper, which the
//! `Lodestar update helper` task runs as SYSTEM, on demand and at boot
//! (plan Q13, owner §2.47). The steps are in
//! [`crate::update::windows::helper`].
//!
//! - It reads no config and no `LODESTAR_*` variable: `--config` is
//!   refused, and the data folder is always the service's default.
//! - `--run-msi` is the helper's own copy in staging, which runs one
//!   `msiexec` while the install folder isn't in use (PK-win-upd-3).
//! - `--register-task` is the MSI's: it registers the task
//!   ([`msi::task_xml`]) as the installer's custom action, as SYSTEM.
//! - Exit status: 0 installed or nothing to do, 1 the install failed,
//!   Windows Installer was busy or the new service isn't healthy, 2 bad
//!   arguments or not Windows, 3 refused. Task Scheduler shows it as the
//!   task's last result; the detail is in `<data>\updates\result.json`
//!   for every request the helper read.

use std::path::Path;
use std::process::ExitCode;

use clap::Args;

#[cfg(windows)]
use crate::update::windows::{helper, msi};

#[derive(Debug, Args, PartialEq, Eq)]
pub struct UpdateHelperArgs {
    /// Register the scheduled task (the installer runs this as SYSTEM)
    #[arg(long, hide = true)]
    pub register_task: bool,
    /// The helper's own copy in staging: run the journal's pending msiexec
    /// (the helper starts this; it refuses anywhere else)
    #[arg(long, hide = true, conflicts_with = "register_task")]
    pub run_msi: bool,
}

pub fn run(args: &UpdateHelperArgs, config_path: Option<&Path>) -> ExitCode {
    if config_path.is_some() {
        eprintln!("lodestar update-helper: takes no --config");
        return ExitCode::from(2);
    }
    #[cfg(windows)]
    {
        if args.run_msi {
            return copy();
        }
        if args.register_task {
            return match register() {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("lodestar update-helper: registering the task: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        install()
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        eprintln!("lodestar update-helper: the MSI's, on Windows only");
        ExitCode::from(2)
    }
}

/// One run of the helper, as the task starts it.
#[cfg(windows)]
fn install() -> ExitCode {
    use helper::{Context, Outcome};
    let refused = |why: &dyn std::fmt::Display| {
        eprintln!("lodestar update-helper: refused: {why}");
        ExitCode::from(3)
    };
    let Ok(exe) = std::env::current_exe() else {
        return refused(&msi::Refusal::NotInstalled);
    };
    let embedded = match lodestar_update::Embedded::builtin() {
        Ok(e) => e,
        Err(e) => return refused(&e.code()),
    };
    let ctx = match Context::new(
        &exe,
        msi::system_program_files().as_deref(),
        std::env::var("ProgramData").ok(),
        env!("LODESTAR_BUILD_VERSION"),
        lodestar_update::Target::from_triple(env!("LODESTAR_BUILD_TARGET")),
        crate::unix_now().max(0) as u64,
        embedded,
    ) {
        Ok(c) => c,
        Err(r) => return refused(&r),
    };
    match helper::run(&ctx, &helper::Windows) {
        Err(r) => refused(&r),
        Ok(o) => {
            eprintln!("lodestar update-helper: {}", o.code());
            match o {
                Outcome::Nothing | Outcome::Installed { .. } | Outcome::HandedOff => {
                    ExitCode::SUCCESS
                }
                Outcome::CopyRefused { .. } => ExitCode::FAILURE,
                Outcome::Failed { .. }
                | Outcome::InstallerBusy { .. }
                | Outcome::RolledBack { .. }
                | Outcome::RollbackPending { .. }
                | Outcome::RollbackHalted { .. }
                | Outcome::InstallInterrupted { .. } => ExitCode::FAILURE,
            }
        }
    }
}

/// The helper's copy (`update-helper --run-msi`): refused unless it's a
/// `run-<hex>.exe` in `%ProgramFiles%\Lodestar\updates`. It waits for the
/// program in Program Files to end (so Windows Installer can replace it),
/// runs the journal's pending `msiexec` and nothing else, then starts the
/// task again so the helper finishes from its answer.
#[cfg(windows)]
fn copy() -> ExitCode {
    let refused = |why: &dyn std::fmt::Display| {
        eprintln!("lodestar update-helper: refused: {why}");
        ExitCode::from(3)
    };
    let Ok(exe) = std::env::current_exe() else {
        return refused(&msi::Refusal::NotInstalled);
    };
    let embedded = match lodestar_update::Embedded::builtin() {
        Ok(e) => e,
        Err(e) => return refused(&e.code()),
    };
    let ctx = match helper::Context::for_copy(
        &exe,
        msi::system_program_files().as_deref(),
        std::env::var("ProgramData").ok(),
        env!("LODESTAR_BUILD_VERSION"),
        lodestar_update::Target::from_triple(env!("LODESTAR_BUILD_TARGET")),
        crate::unix_now().max(0) as u64,
        embedded,
    ) {
        Ok(c) => c,
        Err(r) => return refused(&r),
    };
    // Its own image, read-only and sharing only reading, for the whole run.
    let Ok(_held) = helper::hold_read_only(&exe) else {
        return refused(&"the copy can't hold its own file");
    };
    let name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let installed = exe
        .parent()
        .and_then(Path::parent)
        .map(|d| d.join("lodestar.exe"));
    let wait = || installed.as_deref().is_some_and(wait_free);
    let code = match helper::run_copy(&ctx, &helper::Windows, &name, &wait) {
        Ok(_) => ExitCode::SUCCESS,
        Err(r) => refused(&r),
    };
    // The next run finishes from the answer (or the journal).
    let _ = msi::start_task();
    code
}

/// Waits (up to [`helper::FREE_WAIT`]) until `exe` is no longer running:
/// opening a running image for writing fails. Nothing is written.
#[cfg(windows)]
fn wait_free(exe: &Path) -> bool {
    let deadline = std::time::Instant::now() + helper::FREE_WAIT;
    loop {
        if std::fs::OpenOptions::new().write(true).open(exe).is_ok() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Registers the task for this program, which must be the installed one.
#[cfg(windows)]
fn register() -> Result<(), String> {
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let signals = crate::update::windows::check::Signals {
        service_account: true,
        exe: Some(exe.clone()),
        program_files: msi::system_program_files(),
        ..Default::default()
    };
    if crate::update::windows::check::detect(&signals)
        != crate::update::windows::check::InstallKind::Msi
    {
        return Err(msi::Refusal::NotInstalled.to_string());
    }
    let sid = crate::windows::acl::service_sid(crate::windows::service::NAME)
        .map_err(|e| e.to_string())?;
    let xml = msi::task_xml(&exe, &sid)?;
    let root = std::env::var_os("SystemRoot").ok_or("SystemRoot isn't set")?;
    let mut cmd = std::process::Command::new(
        Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
    );
    helper::clean_env(&mut cmd);
    let mut child = cmd
        .creation_flags(0x0800_0000)
        .env_remove("PSModulePath")
        .env("LODESTAR_TASK", msi::TASK)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            msi::REGISTER_SCRIPT,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(xml.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}
