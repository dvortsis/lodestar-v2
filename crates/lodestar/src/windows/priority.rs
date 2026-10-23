//! Process priority (architecture §11): on Windows Lodestar runs at
//! below-normal priority by default, so the PC stays responsive while it
//! crawls and indexes. `LODESTAR_PRIORITY=normal` keeps the usual one.
//!
//! The workspace forbids `unsafe`, so the priority is set through
//! PowerShell from System32 (the same approach as the key file's ACL,
//! which uses `icacls`), off the start path.

/// The setting's environment variable.
pub const ENV: &str = "LODESTAR_PRIORITY";

/// A Windows priority class Lodestar may run at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    BelowNormal,
    Normal,
}

impl Priority {
    /// `LODESTAR_PRIORITY`: `below_normal` (the default) or `normal`.
    pub fn from_env(value: Option<&str>) -> Result<Priority, String> {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            None | Some("" | "below_normal" | "belownormal" | "below-normal") => {
                Ok(Priority::BelowNormal)
            }
            Some("normal") => Ok(Priority::Normal),
            Some(other) => Err(format!(
                "{ENV}: unknown priority {other:?} (use below_normal or normal)"
            )),
        }
    }

    /// The `ProcessPriorityClass` name PowerShell takes and reports.
    pub fn class(self) -> &'static str {
        match self {
            Priority::BelowNormal => "BelowNormal",
            Priority::Normal => "Normal",
        }
    }
}

/// The PowerShell command that sets process `pid` to `priority`.
pub fn script(pid: u32, priority: Priority) -> String {
    format!(
        "(Get-Process -Id {pid}).PriorityClass = '{}'",
        priority.class()
    )
}

/// Sets process `pid` to `priority`.
#[cfg(windows)]
pub fn set(pid: u32, priority: Priority) -> Result<(), String> {
    run(&script(pid, priority)).map(drop)
}

/// The priority class of process `pid`, as PowerShell names it.
#[cfg(all(windows, test))]
pub fn get(pid: u32) -> Result<String, String> {
    run(&format!("(Get-Process -Id {pid}).PriorityClass")).map(|s| s.trim().to_string())
}

#[cfg(windows)]
fn run(script: &str) -> Result<String, String> {
    let mut cmd = super::powershell_command().ok_or("SystemRoot isn't set")?;
    let out = cmd
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .output()
        .map_err(|e| format!("running PowerShell: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Applies the configured priority to this process, logging the outcome.
#[cfg(windows)]
pub fn apply() {
    let priority = match Priority::from_env(std::env::var(ENV).ok().as_deref()) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("priority: {e}; keeping below normal");
            Priority::BelowNormal
        }
    };
    if priority == Priority::Normal {
        return;
    }
    match set(std::process::id(), priority) {
        Ok(()) => tracing::info!("priority: running at below-normal priority"),
        Err(e) => tracing::warn!("priority: couldn't lower the process priority: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_setting() {
        assert_eq!(Priority::from_env(None), Ok(Priority::BelowNormal));
        assert_eq!(Priority::from_env(Some("")), Ok(Priority::BelowNormal));
        assert_eq!(
            Priority::from_env(Some(" Below_Normal ")),
            Ok(Priority::BelowNormal)
        );
        assert_eq!(Priority::from_env(Some("NORMAL")), Ok(Priority::Normal));
        let err = Priority::from_env(Some("high")).unwrap_err();
        assert!(err.contains("LODESTAR_PRIORITY") && err.contains("\"high\""));
    }

    #[test]
    fn the_script_names_the_process_and_class() {
        assert_eq!(
            script(4242, Priority::BelowNormal),
            "(Get-Process -Id 4242).PriorityClass = 'BelowNormal'"
        );
    }

    #[cfg(windows)]
    #[test]
    fn lowers_another_process() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
            .spawn()
            .unwrap();
        let pid = child.id();
        let result = set(pid, Priority::BelowNormal).and_then(|()| get(pid));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(result.as_deref(), Ok("BelowNormal"));
    }
}
