//! The Windows runtime (architecture §11): the service entrypoint,
//! below-normal priority and the ephemeral-port warning.
//!
//! `main` calls [`hook`] first. Elsewhere it's a no-op; the parsing and
//! policy code compiles and is tested on every OS, and only the parts
//! that touch Windows are `cfg(windows)`.
//!
//! The rest of §11's Windows list lives with the code it concerns:
//! `SO_LINGER(0)` on outbound peer sockets in `lodestar-peer-wire`, and
//! retried renames and deletes of mapped index files in `lodestar-search`
//! (B5a/B5b).

#![cfg_attr(not(windows), allow(dead_code))]

pub mod acl;
pub mod logfile;
pub mod ports;
pub mod priority;
pub mod service;
pub mod temp_log;

use std::process::ExitCode;

/// `main`'s hook, before its own argument parsing. For a serve command
/// line (none, `serve`, `--config <path>`) it runs the whole service when
/// the Service Control Manager started this process, and returns its exit
/// code; otherwise it starts the [`startup`] checks and returns `None` so
/// `main` carries on. Any other command, or arguments that don't parse
/// (`main` reports those), is left alone.
pub fn hook() -> Option<ExitCode> {
    #[cfg(windows)]
    {
        let config_path = service::service_command(std::env::args_os())?;
        if let Some(code) = service::run_if_launched(config_path.clone()) {
            return Some(code);
        }
        // An unreadable config is `main`'s to report.
        if let Ok(config) =
            crate::config::Config::load(config_path.as_deref(), |k| std::env::var(k).ok())
        {
            startup(&config);
        }
    }
    None
}

/// Lowers the process priority and checks the port range, on a thread of
/// its own: both run a system tool, which shouldn't hold up the start.
/// They log once a subscriber is installed (`main` installs it just after
/// [`hook`]).
#[cfg(windows)]
pub fn startup(config: &crate::config::Config) {
    let max_sockets = config.pipeline_config().map_or(0, |p| p.pool.max_sockets);
    let _ = std::thread::Builder::new()
        .name("windows-startup".into())
        .spawn(move || {
            wait_for_subscriber();
            priority::apply();
            if max_sockets > 0 {
                ports::check(max_sockets);
            }
        });
}

/// Waits up to 5 s for the global subscriber.
#[cfg(windows)]
fn wait_for_subscriber() {
    for _ in 0..100 {
        if tracing::dispatcher::has_been_set() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// A command for a tool in `%SystemRoot%\System32`, by absolute path (a
/// tool found through the search path could be anyone's), that opens no
/// console window.
#[cfg(windows)]
fn system32_command(relative: &str) -> Option<std::process::Command> {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NO_WINDOW`.
    const NO_WINDOW: u32 = 0x0800_0000;
    let root = std::env::var_os("SystemRoot")?;
    let mut cmd =
        std::process::Command::new(std::path::Path::new(&root).join("System32").join(relative));
    cmd.creation_flags(NO_WINDOW);
    Some(cmd)
}

/// Windows PowerShell 5.1 from System32, with its own module path: one
/// inherited from PowerShell 7 (a parent shell, a CI runner) points at
/// modules 5.1 can't load, and `Get-Acl` then fails to autoload.
#[cfg(windows)]
fn powershell_command() -> Option<std::process::Command> {
    let mut cmd = system32_command(r"WindowsPowerShell\v1.0\powershell.exe")?;
    cmd.env_remove("PSModulePath");
    Some(cmd)
}
