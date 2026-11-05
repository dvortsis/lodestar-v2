//! Transparent huge pages off by default (X-thp-off,
//! `docs/decisions/2026-11-04-X-thp-off.md`).
//!
//! With the host's THP mode at `always`, or at `madvise` while mimalloc
//! advises `MADV_HUGEPAGE` on its arenas (its `allow_thp` default),
//! khugepaged collapses sparsely used 2 MiB ranges into huge pages, so the
//! worker's resident memory grows when its heap doesn't. Two switches turn
//! that off, Linux only:
//!
//! 1. **`prctl(PR_SET_THP_DISABLE, 1)`**, first thing in `main`. It marks
//!    the whole process (its `mm`, not one thread), is inherited across
//!    `fork` and kept across `execve`, so the supervisor's `--child` and any
//!    re-exec carry it too. From then on the kernel gives no new huge page
//!    to any mapping, old or new (khugepaged included), and ignores
//!    `MADV_HUGEPAGE`. Huge pages already faulted in before `main` stay
//!    until they're freed. This is the switch that takes effect.
//! 2. **`MIMALLOC_ALLOW_THP=0`** in the child's environment. mimalloc
//!    initialises before `main` (the runtime allocates first), and setting
//!    the option from Rust needs `unsafe`, which the workspace forbids, so
//!    it can only reach a process that's about to start. The child's
//!    mimalloc then stops advising `MADV_HUGEPAGE` and sets
//!    PR_SET_THP_DISABLE itself, so it holds even if (1) ever failed. It
//!    covers mimalloc's arenas: in musl builds (the release builds) those
//!    hold the C heaps (SQLite, zstd) too, since mimalloc is also the C
//!    library's `malloc` there (X-mimalloc-override); in other builds the C
//!    library's own heap is outside them, and only (1) covers it.
//!
//! `LODESTAR_THP=allow` is the escape hatch: neither switch is touched, as
//! before this change.

/// The escape hatch's environment variable.
pub const ENV: &str = "LODESTAR_THP";

/// mimalloc's own option, set in the worker's environment.
#[cfg(unix)]
pub const MIMALLOC_ENV: &str = "MIMALLOC_ALLOW_THP";

/// Whether the process may use transparent huge pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The default: THP off for the process and its children.
    Off,
    /// `LODESTAR_THP=allow`: the kernel's and mimalloc's defaults.
    Allow,
}

/// What `LODESTAR_THP` asks for, from its value (`None` if unset). Case and
/// surrounding space are ignored. A value that isn't recognised is the
/// default, `Off`, returned with the value so the caller can warn.
pub fn mode(value: Option<&str>) -> (Mode, Option<&str>) {
    match value.map(str::trim) {
        None | Some("") => (Mode::Off, None),
        Some(v) if v.eq_ignore_ascii_case("off") => (Mode::Off, None),
        Some(v) if v.eq_ignore_ascii_case("allow") => (Mode::Allow, None),
        Some(v) => (Mode::Off, Some(v)),
    }
}

/// The mode from this process's environment.
#[cfg(unix)]
pub fn from_env() -> Mode {
    mode(std::env::var(ENV).ok().as_deref()).0
}

/// The variable to add to the worker's environment for `mode`, if any:
/// Linux only, as THP is (the supervisor also runs on macOS).
#[cfg(unix)]
pub fn child_env(mode: Mode) -> Option<(&'static str, &'static str)> {
    match mode {
        Mode::Off if cfg!(target_os = "linux") => Some((MIMALLOC_ENV, "0")),
        Mode::Off | Mode::Allow => None,
    }
}

/// Applies `LODESTAR_THP` to this process: called first in `main`, before
/// any thread is spawned. Problems are printed, never fatal: THP only
/// costs memory.
pub fn apply() -> Mode {
    let value = std::env::var(ENV).ok();
    let (mode, unknown) = mode(value.as_deref());
    // The supervisor has already warned; its worker inherits the value.
    if let Some(v) = unknown.filter(|_| !is_worker()) {
        eprintln!("{ENV}: {v:?} isn't `allow` or `off`; transparent huge pages stay off");
    }
    if mode == Mode::Off
        && let Err(e) = disable()
    {
        eprintln!("{ENV}: can't turn transparent huge pages off: {e}");
    }
    mode
}

/// Whether this is the supervisor's `--child`, which it marks with its hint.
#[cfg(unix)]
fn is_worker() -> bool {
    std::env::var_os(crate::update::control::ENV).is_some()
}

#[cfg(not(unix))]
fn is_worker() -> bool {
    false
}

/// Sets PR_SET_THP_DISABLE for the whole process.
#[cfg(target_os = "linux")]
fn disable() -> std::io::Result<()> {
    rustix::thread::disable_transparent_huge_pages(true).map_err(Into::into)
}

/// Other systems have no Linux THP to turn off.
#[cfg(not(target_os = "linux"))]
fn disable() -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_empty_is_off() {
        assert_eq!(mode(None), (Mode::Off, None));
        assert_eq!(mode(Some("")), (Mode::Off, None));
        assert_eq!(mode(Some("  ")), (Mode::Off, None));
    }

    #[test]
    fn allow_and_off_in_any_case() {
        for v in ["allow", "ALLOW", " Allow \n"] {
            assert_eq!(mode(Some(v)), (Mode::Allow, None), "{v:?}");
        }
        for v in ["off", "OFF", " Off "] {
            assert_eq!(mode(Some(v)), (Mode::Off, None), "{v:?}");
        }
    }

    #[test]
    fn anything_else_is_off_and_reported() {
        for v in ["1", "0", "on", "always", "madvise", "allowed", "yes"] {
            assert_eq!(mode(Some(v)), (Mode::Off, Some(v)), "{v:?}");
        }
        // The value is reported trimmed.
        assert_eq!(mode(Some(" never ")), (Mode::Off, Some("never")));
    }

    #[cfg(unix)]
    #[test]
    fn the_worker_gets_mimallocs_option_only_when_off() {
        let off = cfg!(target_os = "linux").then_some(("MIMALLOC_ALLOW_THP", "0"));
        assert_eq!(child_env(Mode::Off), off);
        assert_eq!(child_env(Mode::Allow), None);
    }

    /// The flag is process-wide and only ever set here, so setting it in the
    /// test process is harmless.
    #[cfg(target_os = "linux")]
    #[test]
    fn disable_sets_the_process_flag() {
        disable().unwrap();
        assert!(rustix::thread::transparent_huge_pages_are_disabled().unwrap());
        // Every thread sees it: it belongs to the address space.
        let other = std::thread::spawn(rustix::thread::transparent_huge_pages_are_disabled)
            .join()
            .unwrap()
            .unwrap();
        assert!(other);
    }
}
