//! `lodestar update-helper` (hidden): the MSI's update helper, which the
//! `Lodestar update helper` task runs as SYSTEM, on demand and at boot
//! (plan Q13, owner §2.47). The steps are in
//! [`crate::update::windows::helper`].
//!
//! - It reads no config and no `LODESTAR_*` variable: `--config` is
//!   refused, and the data folder is always the service's default.
//! - `--register-task` is the MSI's: it registers the task
//!   ([`msi::task_xml`]) as the installer's custom action, as SYSTEM.
//! - Exit status: 0 installed or nothing to do, 1 the install failed or
//!   the new service isn't healthy, 2 bad arguments or not Windows, 3
//!   refused (nothing was changed). Task Scheduler shows it as the
//!   task's last result.

use std::path::Path;
use std::process::ExitCode;

use clap::Args;

#[allow(unused_imports)]
use crate::update::windows::{helper, msi};

#[derive(Debug, Args, PartialEq, Eq)]
pub struct UpdateHelperArgs {
    /// Register the scheduled task (the installer runs this as SYSTEM)
    #[arg(long, hide = true)]
    pub register_task: bool,
}

pub fn run(args: &UpdateHelperArgs, config_path: Option<&Path>) -> ExitCode {
    if config_path.is_some() {
        eprintln!("lodestar update-helper: takes no --config");
        return ExitCode::from(2);
    }
    #[cfg(windows)]
    {
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
                Outcome::Nothing | Outcome::Installed { .. } => ExitCode::SUCCESS,
                Outcome::Failed { .. } | Outcome::Unhealthy { .. } => ExitCode::FAILURE,
            }
        }
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
