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

pub mod logfile;
pub mod ports;
pub mod priority;
pub mod service;

use std::process::ExitCode;

/// `main`'s hook. Runs the whole service when the Service Control
/// Manager started this process and returns its exit code. Otherwise,
/// for `serve`, starts the [`startup`] checks and returns `None` so
/// `main` carries on.
pub fn hook() -> Option<ExitCode> {
    #[cfg(windows)]
    {
        if let Some(code) = service::run_if_launched() {
            return Some(code);
        }
        if let Ok(crate::Command::Serve(path)) = crate::parse_args(std::env::args().skip(1)) {
            // An unreadable config is `main`'s to report.
            if let Ok(config) =
                crate::config::Config::load(path.as_deref(), |k| std::env::var(k).ok())
            {
                startup(&config);
            }
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
